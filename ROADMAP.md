# pg_trickle — Project Roadmap

> **Last updated:** 2026-03-26
> **Latest release:** 0.10.0 (2026-03-25)
> **Current milestone:** v0.11.0 — Partitioned Stream Tables, Prometheus & Grafana Observability, Safety Hardening & Correctness
> **v0.11.0 progress:** Phase 1 ✅ (PR #279) · Phase 2 ✅ · Phase 3 ✅ (PR #282, Prometheus/Grafana) · Phase 4 ✅ (PR #283, correctness guards) · Phase 5 ✅ (PR #284, VARBIT bitmask) · Phase 6 ✅ (PR #285, fuse circuit breaker) · Phase 7 ✅ (PR #286, event-driven wake)

For a concise description of what pg_trickle is and why it exists, read
[ESSENCE.md](ESSENCE.md) — it explains the core problem (full `REFRESH
MATERIALIZED VIEW` recomputation), how the differential dataflow approach
solves it, the hybrid trigger→WAL CDC architecture, and the broad SQL
coverage, all in plain language.

## Table of Contents

<!-- TOC start -->
- [Overview](#overview)
- [v0.1.x Series — Released](#v01x-series--released)
- [v0.2.0 — TopK, Diamond Consistency & Transactional IVM](#v020--topk-diamond-consistency--transactional-ivm)
- [v0.2.1 — Upgrade Infrastructure & Documentation](#v021--upgrade-infrastructure--documentation)
- [v0.2.2 — OFFSET, AUTO Mode, ALTER QUERY, Edge Cases & CDC Hardening](#v022--offset-auto-mode-alter-query-edge-cases--cdc-hardening)
- [v0.2.3 — Non-Determinism, CDC/Mode Gaps & Operational Polish](#v023--non-determinism-cdcmode-gaps--operational-polish)
- [v0.3.0 — DVM Correctness, SAST & Test Coverage](#v030--dvm-correctness-sast--test-coverage)
- [v0.4.0 — Parallel Refresh & Performance Hardening](#v040--parallel-refresh--performance-hardening)
- [v0.5.0 — Row-Level Security & Operational Controls](#v050--row-level-security--operational-controls)
- [v0.6.0 — Partitioning, Idempotent DDL, Edge Cases & Circular Dependency Foundation](#v060--partitioning-idempotent-ddl-edge-cases--circular-dependency-foundation)
- [v0.7.0 — Performance, Watermarks, Circular DAG Execution, Observability & Infrastructure](#v070--performance-watermarks-circular-dag-execution-observability--infrastructure)
- [v0.8.0 — pg_dump Support & Test Hardening](#v080--pg_dump-support--test-hardening)
- [v0.9.0 — Incremental Aggregate Maintenance](#v090--incremental-aggregate-maintenance)
- [v0.10.0 — DVM Hardening, Connection Pooler Compatibility, Core Refresh Optimizations & Infrastructure Prep](#v0100--dvm-hardening-connection-pooler-compatibility-core-refresh-optimizations--infrastructure-prep)
- [v0.11.0 — Partitioned Stream Tables, Prometheus & Grafana Observability, Safety Hardening & Correctness](#v0110--partitioned-stream-tables-prometheus--grafana-observability-safety-hardening--correctness)
- [v0.12.0 — Scalability Foundations, Anomalous Change Detection & CDC Research](#v0120--scalability-foundations-anomalous-change-detection--cdc-research)
- [v0.13.0 — Tiered Scheduling, PG Backward Compatibility & UNLOGGED Buffers](#v0130--tiered-scheduling-pg-backward-compatibility--unlogged-buffers)
- [v0.14.0 — Native DDL Syntax, External Test Suites & Integration](#v0140--native-ddl-syntax-external-test-suites--integration)
- [v1.0.0 — Stable Release](#v100--stable-release)
- [Post-1.0 — Scale & Ecosystem](#post-10--scale--ecosystem)
- [Effort Summary](#effort-summary)
- [References](#references)
<!-- TOC end -->

---

## Overview

pg_trickle is a PostgreSQL 18 extension that implements streaming tables with
incremental view maintenance (IVM) via differential dataflow. The extension is
designed for **maximum performance, low latency, and high throughput** —
differential refresh is the default mode, and full refresh is a fallback of
last resort. All 13 design phases are complete. This roadmap tracks the path
from the v0.1.x series to 1.0 and beyond.

```
                                                                                                                              We are here
                                                                                                                                │
                                                                                                                                ▼
                                                                   ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
                                                                   │ 0.1.x  │ │ 0.2.0  │ │ 0.2.1  │ │ 0.2.2  │ │ 0.2.3  │ │ 0.3.0  │ │ 0.4.0  │ │ 0.5.0  │ │ 0.6.0  │ │ 0.7.0  │
                                                                   │Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│
                                                                   │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │
                                                                   └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘
                                                                     │
                                                                     └─ ┌────────┐ ┌────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐
                                                                        │ 0.8.0  │ │ 0.9.0  │ │ 0.10.0  │ │ 0.11.0  │ │ 0.12.0  │
                                                                        │Released│─│Released│─│Released │─│Partn.   │─│Fuse,    │
                                                                        │ ✅      │ │ ✅      │ │ ✅       │ │Obs.&Sc. │ │CDC&PGBk │
                                                                        └────────┘ └────────┘ └─────────┘ └─────────┘ └─────────┘
              │
              └─ ┌─────────┐ ┌─────────┐ ┌────────┐ ┌────────┐
                 │ 0.13.0  │ │ 0.14.0  │ │ 1.0.0  │ │ 1.x+   │
                 │Perf.Opt │─│DDL,Test │─│Stable  │─│Scale & │
                 │&Scale   │ │&Integ.  │ │Release │ │Ecosys. │
                 └─────────┘ └─────────┘ └────────┘ └────────┘
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
three major feature additions.

<details>
<summary>Completed items (click to expand)</summary>

| Tier | Items | Status |
|------|-------|--------|
| 0 — Critical | F1–F3, F5–F6 | ✅ Done in v0.1.1–v0.1.3 |
| 1 — Verification | F8–F10, F12 | ✅ Done in v0.1.2–v0.1.3 |
| 2 — Robustness | F13, F15–F16 | ✅ Done in v0.1.2–v0.1.3 |
| 3 — Test coverage | F17–F26 (62 E2E tests) | ✅ Done in v0.1.2–v0.1.3 |
| 4 — Operational hardening | F27–F39 | ✅ Done in v0.1.3 |
| 4 — Upgrade migrations | F40 | ✅ Done in v0.2.1 |
| 5 — Nice-to-have | F41–F51 | ✅ Done in v0.1.3 |

**TPC-H baseline:** 22/22 queries pass deterministic correctness checks across
multiple mutation cycles (`just test-tpch`, SF=0.01).

> *Queries are derived from the TPC-H Benchmark specification; results are not
> comparable to published TPC results. TPC Benchmark™ is a trademark of TPC.*

</details>

### ORDER BY / LIMIT / OFFSET — TopK Support ✅

> **In plain terms:** Stream tables can now be defined with `ORDER BY ... LIMIT N`
> — for example "keep the top 10 best-selling products". When the underlying data
> changes, only the top-N slot is updated incrementally rather than recomputing
> the entire sorted list from scratch every tick.

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

> **In plain terms:** A "diamond" is when two stream tables share the same source
> (A → B, A → C) and a third (D) reads from both B and C. Without special
> handling, updating A could refresh B before C, leaving D briefly in an
> inconsistent state where it sees new-B but old-C. This groups B and C into an
> atomic refresh unit so D always sees them change together in a single step.

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

> **In plain terms:** Normally stream tables refresh on a schedule (every N
> seconds). IMMEDIATE mode updates the stream table *inside the same database
> transaction* as the source table change — so by the time your INSERT/UPDATE/
> DELETE commits, the stream table is already up to date. Zero lag, at the cost
> of a slightly slower write.

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

## v0.2.1 — Upgrade Infrastructure & Documentation

**Status: Released (2026-03-05).**

Patch release focused on upgrade safety, documentation, and three catalog
schema additions via `sql/pg_trickle--0.2.0--0.2.1.sql`:

- `has_keyless_source BOOLEAN NOT NULL DEFAULT FALSE` — EC-06 keyless source
  flag; changes apply strategy from MERGE to counted DELETE when set.
- `function_hashes TEXT` — EC-16 function-body hash map; forces a full
  refresh when a referenced function's body changes silently.
- `topk_offset INT` — OS2 catalog field for paged TopK OFFSET support,
  shipped and used in this release.

### Upgrade Migration Infrastructure ✅

> **In plain terms:** When you run `ALTER EXTENSION pg_trickle UPDATE`, all your
> stream tables should survive intact. This adds the safety net that makes that
> true: automated scripts that check every upgrade script covers all database
> objects, real end-to-end tests that actually perform the upgrade in a test
> container, and CI gates that catch regressions before they reach users.

Complete safety net for `ALTER EXTENSION pg_trickle UPDATE`:

| Item | Description | Status |
|------|-------------|--------|
| U1 | `scripts/check_upgrade_completeness.sh` — CI completeness checker | ✅ Done |
| U2 | `sql/archive/` with archived SQL baselines per version | ✅ Done |
| U3 | `tests/Dockerfile.e2e-upgrade` for real upgrade tests | ✅ Done |
| U4 | 6 upgrade E2E tests (function parity, stream table survival, etc.) | ✅ Done |
| U5 | CI: `upgrade-check` (every PR) + `upgrade-e2e` (push-to-main) | ✅ Done |
| U6 | `docs/UPGRADING.md` user-facing upgrade guide | ✅ Done |
| U7 | `just check-upgrade`, `just build-upgrade-image`, `just test-upgrade` | ✅ Done |
| U8 | Fixed 0.1.3→0.2.0 upgrade script (was no-op placeholder) | ✅ Done |

### Documentation Expansion ✅

> **In plain terms:** Added six new pages to the documentation book: a dbt
> integration guide, contributing guide, security policy, release process, a
> comparison with the pg_ivm extension, and a deep-dive explaining why
> row-level triggers were chosen over logical replication for CDC.

GitHub Pages book grew from 14 to 20 pages:

| Page | Section | Source |
|------|---------|--------|
| dbt Integration | Integrations | `dbt-pgtrickle/README.md` |
| Contributing | Reference | `CONTRIBUTING.md` |
| Security Policy | Reference | `SECURITY.md` |
| Release Process | Reference | `docs/RELEASE.md` |
| pg_ivm Comparison | Research | `plans/ecosystem/GAP_PG_IVM_COMPARISON.md` |
| Triggers vs Replication | Research | `plans/sql/REPORT_TRIGGERS_VS_REPLICATION.md` |

**Exit criteria:**
- [x] `ALTER EXTENSION pg_trickle UPDATE` from 0.1.3→0.2.0 tested end-to-end
- [x] Completeness check passes (upgrade script covers all pgrx-generated SQL objects)
- [x] CI enforces upgrade script completeness on every PR
- [x] All documentation pages build and render in mdBook

---

## v0.2.2 — OFFSET, AUTO Mode, ALTER QUERY, Edge Cases & CDC Hardening

**Status: Released (2026-03-08).**

This milestone shipped paged TopK OFFSET support, AUTO-by-default refresh
selection, ALTER QUERY, the remaining upgrade-tooling work, edge-case and WAL
CDC hardening, IMMEDIATE-mode parity fixes, and the outstanding documentation
sweep.

### ORDER BY + LIMIT + OFFSET (Paged TopK) — Finalization ✅

> **In plain terms:** Extends TopK to support OFFSET — so you can define a
> stream table as "rows 11–20 of the top-20 best-selling products" (page 2 of
> a ranked list). Useful for paginated leaderboards, ranked feeds, or any
> use case where you want a specific window into a sorted result.

Core implementation is complete (parser, catalog, refresh path, docs, 9 E2E
tests). The `topk_offset` catalog column shipped in v0.2.1 and is exercised
by the paged TopK feature here.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| OS1 | 9 OFFSET E2E tests in `e2e_topk_tests.rs` | ✅ Done | [PLAN_OFFSET_SUPPORT.md](plans/sql/PLAN_OFFSET_SUPPORT.md) §Step 6 |
| OS2 | `sql/pg_trickle--0.2.1--0.2.2.sql` — function signature updates (no schema DDL needed) | ✅ Done | [PLAN_OFFSET_SUPPORT.md](plans/sql/PLAN_OFFSET_SUPPORT.md) §Step 2 |

### AUTO Refresh Mode ✅

> **In plain terms:** Changes the default from "always try differential
> (incremental) refresh" to a smart automatic selection: use differential when
> the query supports it, fall back to a full re-scan when it doesn't. New stream
> tables also get a calculated schedule interval instead of a hardcoded
> 1-minute default.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| AM1 | `RefreshMode::Auto` — uses DIFFERENTIAL when supported, falls back to FULL | ✅ Done | [PLAN_REFRESH_MODE_DEFAULT.md](plans/sql/PLAN_REFRESH_MODE_DEFAULT.md) |
| AM2 | `create_stream_table` default changed from `'DIFFERENTIAL'` to `'AUTO'` | ✅ Done | — |
| AM3 | `create_stream_table` schedule default changed from `'1m'` to `'calculated'` | ✅ Done | — |

### ALTER QUERY ✅

> **In plain terms:** Lets you change the SQL query of an existing stream table
> without dropping and recreating it. pg_trickle inspects the old and new
> queries, determines what type of change was made (added a column, dropped a
> column, or fundamentally incompatible change), and performs the most minimal
> migration possible — updating in place where it can, rebuilding only when it
> must.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| AQ1 | `alter_stream_table(query => ...)` — validate, classify schema change, migrate storage | ✅ Done | [PLAN_ALTER_QUERY.md](plans/PLAN_ALTER_QUERY.md) |
| AQ2 | Schema classification: same, compatible (ADD/DROP COLUMN), incompatible (full rebuild) | ✅ Done | — |
| AQ3 | ALTER-aware cycle detection (`check_for_cycles_alter`) | ✅ Done | — |
| AQ4 | CDC dependency migration (add/remove triggers for changed sources) | ✅ Done | — |
| AQ5 | SQL Reference & CHANGELOG documentation | ✅ Done | — |

### Upgrade Tooling ✅

> **In plain terms:** If the compiled extension library (`.so` file) is a
> different version than the SQL objects in the database, the scheduler now
> warns loudly at startup instead of failing in confusing ways later. Also
> adds FAQ entries and cross-links for common upgrade questions.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| UG1 | Version mismatch check — scheduler warns if `.so` version ≠ SQL version | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) §5.2 |
| UG2 | FAQ upgrade section — 3 new entries with UPGRADING.md cross-links | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) §5.4 |
| UG3 | CI and local upgrade automation now target 0.2.2 (`upgrade-check`, upgrade-image defaults, upgrade E2E env) | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) |

### IMMEDIATE Mode Parity ✅

> **In plain terms:** Closes two remaining SQL patterns that worked in
> DIFFERENTIAL mode but not in IMMEDIATE mode. Recursive CTEs (queries that
> reference themselves to compute e.g. graph reachability or org-chart
> hierarchies) now work in IMMEDIATE mode with a configurable depth guard.
> TopK (ORDER BY + LIMIT) queries also get a dedicated fast micro-refresh path
> in IMMEDIATE mode.

Close the gap between DIFFERENTIAL and IMMEDIATE mode SQL coverage for the
two remaining high-risk patterns — recursive CTEs and TopK queries.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| IM1 | Validate recursive CTE semi-naive in IMMEDIATE mode; add stack-depth guard for deeply recursive defining queries | 2–3d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 6 §5.1 | ✅ Done — `check_for_delete_changes` handles `TransitionTable`; `generate_change_buffer_from` uses NEW transition table in IMMEDIATE mode; `ivm_recursive_max_depth` GUC (default 100) injects `__pgt_depth` counter into semi-naive SQL |
| IM2 | TopK in IMMEDIATE mode: statement-level micro-refresh + `ivm_topk_max_limit` GUC | 2–3d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 6 §5.2 | ✅ Done — `apply_topk_micro_refresh()` in ivm.rs; GUC threshold check in api.rs; 10 E2E tests (basic, insert, delete, update, aggregate, offset, multi-DML, threshold rejection, mode switch) |

> **IMMEDIATE parity subtotal: ✅ Complete (IM1 + IM2)**

### Edge Case Hardening ✅

> **In plain terms:** Three targeted fixes for uncommon-but-real scenarios:
> a cap on CUBE/ROLLUP combinatorial explosion (which can generate thousands
> of grouping variants from a single query and crash the database); automatic
> recovery when CDC gets stuck in a "transitioning" state after a database
> restart; and polling-based change detection for foreign tables (tables in
> external databases) that can't use triggers or WAL.

Self-contained items from Stage 7 of the edge-cases/TIVM implementation plan.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC1 | `pg_trickle.max_grouping_set_branches` GUC — cap CUBE/ROLLUP branch-count explosion | 4h | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-02 | ✅ Done — GUC in config.rs (default 64, range 1–65536); parser.rs rejects when branch count exceeds limit; 3 E2E tests (rejection, within-limit, raised limit) |
| EC2 | Post-restart CDC `TRANSITIONING` health check — detect stuck CDC transitions after crash or restart | 1d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-20 | ✅ Done — `check_cdc_transition_health()` in scheduler.rs; detects missing replication slots; rolls back to TRIGGER mode |
| EC3 | Foreign table support: polling-based change detection via periodic re-execution | 2–3d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-05 | ✅ Done — `pg_trickle.foreign_table_polling` GUC; `setup_foreign_table_polling()` creates snapshot table; `poll_foreign_table_changes()` uses EXCEPT ALL deltas; 3 E2E tests (rejection, FULL mode, polling correctness) |

> **Edge-case hardening subtotal: ✅ Complete (EC1 + EC2 + EC3)**

### Documentation Sweep

> **In plain terms:** Filled three documentation gaps: what happens to an
> in-flight refresh if you run DDL (ALTER TABLE, DROP INDEX) at the same time;
> limitations when using pg_trickle on standby replicas; and a PgBouncer
> configuration guide explaining the session-mode requirement and incompatible
> settings.

Remaining documentation gaps identified in Stage 7 of the gap analysis.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| DS1 | DDL-during-refresh behaviour: document safe patterns and races | 2h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-17 |
| DS2 | Replication/standby limitations: document in FAQ and Architecture | 3h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-21/22/23 |
| DS3 | PgBouncer configuration guide: session-mode requirements and known incompatibilities | 2h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-28 |

> **Documentation sweep subtotal: ✅ Complete**

### WAL CDC Hardening

> **In plain terms:** WAL (Write-Ahead Log) mode tracks changes by reading
> PostgreSQL's internal replication stream rather than using row-level triggers
> — which is more efficient and works across concurrent sessions. This work
> added a complete E2E test suite for WAL mode, hardened the automatic fallback
> from WAL to trigger mode when WAL isn't available, and promoted `cdc_mode =
> 'auto'` (try WAL first, fall back to triggers) as the default.

> WAL decoder F2–F3 fixes (keyless pk_hash, `old_*` columns for UPDATE) landed in v0.1.3.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| W1 | WAL mode E2E test suite (parallel to trigger suite) | 8–12h | ✅ Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W2 | WAL→trigger automatic fallback hardening | 4–6h | ✅ Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |
| W3 | Promote `pg_trickle.cdc_mode = 'auto'` to default | ~1h | ✅ Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) |

> **WAL CDC subtotal: ~13–19 hours**

**Exit criteria:**
- [x] `ORDER BY + LIMIT + OFFSET` defining queries accepted, refreshed, and E2E-tested
- [x] `sql/pg_trickle--0.2.1--0.2.2.sql` exists (column pre-provisioned in 0.2.1; function signature updates)
- [x] Upgrade completeness check passes for 0.2.1→0.2.2
- [x] CI and local upgrade-E2E defaults target 0.2.2
- [x] Version check fires at scheduler startup if `.so`/SQL versions diverge
- [x] IMMEDIATE mode: recursive CTE semi-naive validated; `ivm_recursive_max_depth` depth guard added
- [x] IMMEDIATE mode: TopK micro-refresh fully tested end-to-end (10 E2E tests)
- [x] `max_grouping_set_branches` GUC guards CUBE/ROLLUP explosion (3 E2E tests)
- [x] Post-restart CDC TRANSITIONING health check in place
- [x] Foreign table polling-based CDC implemented (3 E2E tests)
- [x] DDL-during-refresh and standby/replication limitations documented
- [x] WAL CDC mode passes full E2E suite
- [x] E2E tests pass (`just build-e2e-image && just test-e2e`)

---

## v0.2.3 — Non-Determinism, CDC/Mode Gaps & Operational Polish

**Goal:** Close a small set of high-leverage correctness and operational gaps
that do not need to wait for the larger v0.3.0 parallel refresh, security, and
partitioning work.
This milestone tightens refresh-mode behavior, makes CDC transitions easier to
observe, and removes one silent correctness hazard in DIFFERENTIAL mode.

### Non-Deterministic Function Handling

> **In plain terms:** Functions like `random()`, `gen_random_uuid()`, and
> `clock_timestamp()` return a different value every time they're called. In
> DIFFERENTIAL mode, pg_trickle computes *what changed* between the old and
> new result — but if a function changes on every call, the "change" is
> meaningless and produces phantom rows. This detects such functions at
> stream-table creation time and rejects them in DIFFERENTIAL mode (they still
> work fine in FULL or IMMEDIATE mode).

Status: Done. Volatility lookup, OpTree enforcement, E2E coverage, and
documentation are complete.

Volatile functions (`random()`, `gen_random_uuid()`, `clock_timestamp()`) break
delta computation in DIFFERENTIAL mode — values change on each evaluation,
causing phantom changes and corrupted row identity hashes. This is a silent
correctness gap.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ND1 | Volatility lookup via `pg_proc.provolatile` + recursive `Expr` scanner | Done | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Part 1 |
| ND2 | OpTree volatility walker + enforcement policy (reject volatile in DIFFERENTIAL, warn for stable) | Done | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Part 2 |
| ND3 | E2E tests (volatile rejected, stable warned, immutable allowed, nested volatile in WHERE) | Done | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §E2E Tests |
| ND4 | Documentation (`SQL_REFERENCE.md`, `DVM_OPERATORS.md`) | Done | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) §Files |

> **Non-determinism subtotal: ~4–6 hours**

### CDC / Refresh Mode Interaction Gaps ✅

> **In plain terms:** pg_trickle has four CDC modes (trigger, WAL, auto,
> per-table override) and four refresh modes (FULL, DIFFERENTIAL, IMMEDIATE,
> AUTO). Not every combination makes sense, and some had silent bugs. This
> fixed six specific gaps: stale change buffers not being flushed after FULL
> refreshes (so they got replayed again on the next tick), a missing error for
> the IMMEDIATE + WAL combination, a new `pgt_cdc_status` monitoring view,
> per-table CDC mode overrides, and a guard against refreshing stream tables
> that haven't been populated yet.

Six gaps between the four CDC modes and four refresh modes — missing
validations, resource leaks, and observability holes. Phased from quick wins
(pure Rust) to a larger feature (per-table `cdc_mode` override).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G6 | Defensive `is_populated` + empty-frontier check in `execute_differential_refresh()` | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G6 |
| G2 | Validate `IMMEDIATE` + `cdc_mode='wal'` — global-GUC path logs INFO; explicit per-table override is rejected with a clear error | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G2 |
| G3 | Advance WAL replication slot after FULL refresh; flush change buffers | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G3 |
| G4 | Flush change buffers after AUTO→FULL adaptive fallback (prevents ping-pong) | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G4 |
| G5 | `pgtrickle.pgt_cdc_status` view + NOTIFY on CDC transitions | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G5 |
| G1 | Per-table `cdc_mode` override (SQL API, catalog, dbt, migration) | Done | [PLAN_CDC_MODE_REFRESH_MODE_GAPS.md](plans/sql/PLAN_CDC_MODE_REFRESH_MODE_GAPS.md) §G1 |

> **CDC/refresh mode gaps subtotal: ✅ Complete**
>
> **Progress:** G6 is now implemented in `v0.2.3`: the low-level
> differential executor rejects unpopulated stream tables and missing
> frontiers before it can scan from `0/0`, while the public manual-refresh
> path continues to fall back to FULL for `initialize => false` stream tables.
>
> **Progress:** G1 and G2 are now complete: `create_stream_table()` and
> `alter_stream_table()` accept an optional per-table `cdc_mode` override,
> the requested value is stored in `pgt_stream_tables.requested_cdc_mode`, dbt
> forwards the setting, and shared-source WAL transition eligibility is now
> resolved conservatively from all dependent deferred stream tables. The
> cluster-wide `pg_trickle.cdc_mode = 'wal'` path still logs INFO for
> `refresh_mode = 'IMMEDIATE'`, while explicit per-table `cdc_mode => 'wal'`
> requests are rejected for IMMEDIATE mode with a clear error.
>
> **Progress:** G3 and G4 are now implemented in `v0.2.3`:
> `advance_slot_to_current()` in `wal_decoder.rs` advances WAL slots after
> each FULL refresh; the shared `post_full_refresh_cleanup()` helper in
> `refresh.rs` advances all WAL/TRANSITIONING slots and flushes change buffers,
> called from `scheduler.rs` after every Full/Reinitialize execution and from
> the adaptive fallback path. This prevents change-buffer ping-pong on
> bulk-loaded tables.
>
> **Progress:** G5 is now implemented in `v0.2.3`: the
> `pgtrickle.pgt_cdc_status` convenience view has been added, and a
> `cdc_modes` text-array column surfaces per-source CDC modes in
> `pgtrickle.pg_stat_stream_tables`. NOTIFY on CDC transitions
> (TRIGGER → TRANSITIONING → WAL) was already implemented via
> `emit_cdc_transition_notify()` in `wal_decoder.rs`.

> **Progress:** The SQL upgrade path for these CDC and monitoring changes is in
> place via `sql/pg_trickle--0.2.2--0.2.3.sql`, which adds
> `requested_cdc_mode`, updates the `create_stream_table` /
> `alter_stream_table` signatures, recreates `pgtrickle.pg_stat_stream_tables`,
> and adds `pgtrickle.pgt_cdc_status` for `ALTER EXTENSION ... UPDATE` users.

### Operational

> **In plain terms:** Four housekeeping improvements: clean up prepared
> statements when the database catalog changes (prevents stale caches after
> DDL); make WAL slot lag alert thresholds configurable rather than hardcoded;
> simplify a confusing GUC setting (`user_triggers`) with a deprecated alias;
> and add a `pg_trickle_dump` tool that exports all stream table definitions
> to a replayable SQL file — useful as a backup before running an upgrade.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| O1 | Prepared statement cleanup on cache invalidation | Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) G4.4 |
| O2 | Slot lag alerting thresholds configurable (`slot_lag_warning_threshold_mb`, `slot_lag_critical_threshold_mb`) | Done | [PLAN_HYBRID_CDC.md](plans/sql/PLAN_HYBRID_CDC.md) §6.2 |
| O3 | Simplify `pg_trickle.user_triggers` GUC (canonical `auto` / `off`, deprecated `on` alias) | Done | [PLAN_FEATURE_CLEANUP.md](plans/PLAN_FEATURE_CLEANUP.md) C5 |
| O4 | `pg_trickle_dump`: SQL export tool for manual backup before upgrade | Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) §5.3 |

> **Operational subtotal: Done**
>
> **Progress:** All four operational items are now shipped in `v0.2.3`.
> Warning-level and critical WAL slot lag thresholds are configurable,
> prepared `__pgt_merge_*` statements are cleaned up on shared cache
> invalidation, `pg_trickle.user_triggers` is simplified to canonical
> `auto` / `off` semantics with a deprecated `on` alias, and
> `pg_trickle_dump` provides a replayable SQL export for upgrade backups.

> **v0.2.3 total: ~45–66 hours**

**Exit criteria:**
- [x] Volatile functions rejected in DIFFERENTIAL mode; stable functions warned
- [x] DIFFERENTIAL on unpopulated ST returns error (G6)
- [x] IMMEDIATE + explicit `cdc_mode='wal'` rejected with clear error (G2)
- [x] WAL slot advanced after FULL refresh; change buffers flushed (G3)
- [x] Adaptive fallback flushes change buffers; no ping-pong cycles (G4)
- [x] `pgtrickle.pgt_cdc_status` view available; NOTIFY on CDC transitions (G5)
- [x] Prepared statement cache cleanup works after invalidation
- [x] Per-table `cdc_mode` override functional in SQL API and dbt adapter (G1)
- [x] Extension upgrade path tested (`0.2.2 → 0.2.3`)

**Status: Released (2026-03-09).**

---

## v0.3.0 — DVM Correctness, SAST & Test Coverage

**Goal:** Re-enable all 18 previously-ignored DVM correctness E2E tests by
fixing HAVING, FULL OUTER JOIN, correlated EXISTS+HAVING, and correlated scalar
subquery differential computation bugs. Harden the SAST toolchain with
privilege-context rules and an unsafe-block baseline. Expand TPC-H coverage
with rollback, mode-comparison, single-row, and DAG tests.

### DVM Correctness Fixes

> **In plain terms:** The Differential View Maintenance engine — the core
> algorithm that computes *what changed* incrementally — had four correctness
> bugs in specific SQL patterns. Queries using these patterns were silently
> producing wrong results and had their tests marked "ignored". This release
> fixes all four: HAVING clauses on aggregates, FULL OUTER JOINs, correlated
> EXISTS subqueries combined with HAVING, and correlated scalar subqueries in
> SELECT lists. All 18 previously-ignored E2E tests now pass.

| Item | Description | Status |
|------|-------------|--------|
| DC1 | HAVING clause differential correctness — fix `COUNT(*)` rewrite and threshold-crossing upward rescan (5 tests un-ignored) | ✅ Done |
| DC2 | FULL OUTER JOIN differential correctness — fix row-id mismatch, compound GROUP BY expressions, SUM NULL semantics, and rescan CTE SELECT list (5 tests un-ignored) | ✅ Done |
| DC3 | Correlated EXISTS with HAVING differential correctness — fix EXISTS sublink parser discarding GROUP BY/HAVING, row-id mismatch for `Project(SemiJoin)`, and `diff_project` row-id recomputation (1 test un-ignored) | ✅ Done |
| DC4 | Correlated scalar subquery differential correctness — `rewrite_correlated_scalar_in_select` rewrites correlated scalar subqueries to LEFT JOINs before DVM parsing (2 tests un-ignored) | ✅ Done |

> **DVM correctness subtotal: 18 previously-ignored E2E tests re-enabled (0 remaining)**

### SAST Program (Phases 1–3)

> **In plain terms:** Adds formal static security analysis (SAST) to every
> build. CodeQL and Semgrep scan for known vulnerability patterns — for
> example, using SECURITY DEFINER functions without locking down `search_path`,
> or calling `SET ROLE` in ways that could be abused. Separately, every Rust
> `unsafe {}` block is inventoried and counted; any PR that adds new unsafe
> blocks beyond the committed baseline fails CI automatically.

| Item | Description | Status |
|------|-------------|--------|
| S1 | CodeQL + `cargo deny` + initial Semgrep baseline — zero findings across 115 Rust source files | ✅ Done |
| S2 | Narrow `rust.panic-in-sql-path` scope — exclude `src/dvm/**` and `src/bin/**` to eliminate 351 false-positive alerts | ✅ Done |
| S3 | `sql.row-security.disabled` Semgrep rule — flag `SET LOCAL row_security = off` | ✅ Done |
| S4 | `sql.set-role.present` Semgrep rule — flag `SET ROLE` / `RESET ROLE` patterns | ✅ Done |
| S5 | Updated `sql.security-definer.present` message to require explicit `SET search_path` | ✅ Done |
| S6 | `scripts/unsafe_inventory.sh` + `.unsafe-baseline` — per-file `unsafe {` counter with committed baseline (1309 blocks across 6 files) | ✅ Done |
| S7 | `.github/workflows/unsafe-inventory.yml` — advisory CI workflow; fails if any file exceeds its baseline | ✅ Done |
| S8 | Remove `pull_request` trigger from CodeQL + Semgrep workflows (no inline PR annotations; runs on push-to-main + weekly schedule) | ✅ Done |

> **SAST subtotal: Phases 1–3 complete; Phase 4 rule promotion tracked as post-v0.3.0 cleanup**

### TPC-H Test Suite Enhancements (T1–T6)

> **In plain terms:** TPC-H is an industry-standard analytical query benchmark
> — 22 queries against a simulated supply-chain database. This extends the
> pg_trickle TPC-H test suite to verify four additional scenarios that the
> basic correctness checks didn't cover: that ROLLBACK atomically undoes an
> IVM stream table update; that DIFFERENTIAL and IMMEDIATE mode produce
> *identical* answers for the same data; that single-row mutations work
> correctly (not just bulk changes); and that multi-level stream table DAGs
> refresh in the correct topological order.

| Item | Description | Status |
|------|-------------|--------|
| T1 | `__pgt_count < 0` guard in `assert_tpch_invariant` — over-retraction detector, applies to all existing TPC-H tests | ✅ Done |
| T2 | Skip-set regression guard in DIFFERENTIAL + IMMEDIATE tests — any newly skipped query not in the allowlist fails CI | ✅ Done |
| T3 | `test_tpch_immediate_rollback` — verify ROLLBACK restores IVM stream table atomically across RF mutations | ✅ Done |
| T4 | `test_tpch_differential_vs_immediate` — side-by-side comparison: both incremental modes produce identical results after shared mutations | ✅ Done |
| T5 | `test_tpch_single_row_mutations` + SQL fixtures — single-row INSERT/UPDATE/DELETE IVM trigger paths on Q01/Q06/Q03 | ✅ Done |
| T6a | `test_tpch_dag_chain` — two-level DAG (Q01 → filtered projection), refreshed in topological order | ✅ Done |
| T6b | `test_tpch_dag_multi_parent` — multi-parent fan-in (Q01 + Q06 → UNION ALL), DIFFERENTIAL mode | ✅ Done |

> **TPC-H subtotal: T1–T6 complete; 22/22 TPC-H queries passing**

**Exit criteria:**
- [x] All 18 previously-ignored DVM correctness E2E tests re-enabled
- [x] SAST Phases 1–3 deployed; unsafe baseline committed; CodeQL zero findings
- [x] TPC-H T1–T6 implemented; rollback, differential-vs-immediate, single-row, and DAG tests pass
- [x] Extension upgrade path tested (`0.2.3 → 0.3.0`)

**Status: Released (2026-03-11).**

---

## v0.4.0 — Parallel Refresh & Performance Hardening

**Goal:** Deliver true parallel refresh, cut write-side CDC overhead with
statement-level triggers, close a cross-source snapshot consistency gap, and
ship quick ergonomic and infrastructure improvements. Together these close the
main performance and operational gaps before the security and partitioning
work begins.

### Parallel Refresh

> **In plain terms:** Right now the scheduler refreshes stream tables one at
> a time. This feature lets multiple stream tables refresh simultaneously —
> like running several errands at once instead of in a queue. When you have
> dozens of stream tables, this can cut total refresh latency dramatically.

Detailed implementation is tracked in
[PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md). The older
[REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md)
remains the options-analysis precursor.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| P1 | Phase 0–1: instrumentation, `dry_run`, and execution-unit DAG (atomic groups + IMMEDIATE closures) | 12–20h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |
| P2 | Phase 2–4: job table, worker budget, dynamic refresh workers, and ready-queue dispatch | 16–28h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |
| P3 | Phase 5–7: composite units, observability, rollout gating, and CI validation | 12–24h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |

**Progress:**
- [x] **P1 — Phase 0 + Phase 1** (done): GUCs (`parallel_refresh_mode`, `max_dynamic_refresh_workers`), `ExecutionUnit`/`ExecutionUnitDag` types in `dag.rs`, IMMEDIATE-closure collapsing, dry-run logging in scheduler, 10 new unit tests (1211 total).
- [x] **P2 — Phase 2–4** (done): Job table (`pgt_scheduler_jobs`), catalog CRUD, shared-memory token pool (Phase 2). Dynamic worker entry point, spawn helper, reconciliation (Phase 3). Coordinator dispatch loop with ready-queue scheduling, per-db/cluster-wide budget enforcement, transaction-split spawning, dynamic poll interval, 8 new unit tests (Phase 4). 1233 unit tests total.
- [x] **P3a — Phase 5** (done): Composite unit execution — `execute_worker_atomic_group()` with C-level sub-transaction rollback, `execute_worker_immediate_closure()` with root-only refresh (IMMEDIATE triggers propagate downstream). Replaces Phase 3 serial placeholder.
- [x] **P3b — Phase 6** (done): Observability — `worker_pool_status()`, `parallel_job_status()` SQL functions; `health_check()` extended with `worker_pool` and `job_queue` checks; docs updated.
- [x] **P3c — Phase 7** (done): Rollout — GUC documentation in `CONFIGURATION.md`, worker-budget guidance in `ARCHITECTURE.md`, CI E2E coverage with `PGT_PARALLEL_MODE=on`, feature stays gated behind `parallel_refresh_mode = 'off'` default.

> **Parallel refresh subtotal: ~40–72 hours**

### Statement-Level CDC Triggers

> **In plain terms:** Previously, when you updated 1,000 rows in a source
> table, the database fired a "row changed" notification 1,000 times — once
> per row. Now it fires once per statement, handing off all 1,000 changed
> rows in a single batch. For bulk operations like data imports or batch
> updates this is 50–80% cheaper; for single-row changes you won't notice a
> difference.

Replace per-row AFTER triggers with statement-level triggers using
`NEW TABLE AS __pgt_new` / `OLD TABLE AS __pgt_old`. Expected write-side
trigger overhead reduction of 50–80% for bulk DML; neutral for single-row.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~B1~~ | ~~Replace per-row triggers with statement-level triggers; INSERT/UPDATE/DELETE via set-based buffer fill~~ | ~~8h~~ | ✅ Done — `build_stmt_trigger_fn_sql` in cdc.rs; `REFERENCING NEW TABLE AS __pgt_new OLD TABLE AS __pgt_old FOR EACH STATEMENT` created by `create_change_trigger` |
| ~~B2~~ | ~~`pg_trickle.cdc_trigger_mode = 'statement'\|'row'` GUC + migration to replace row-level triggers on `ALTER EXTENSION UPDATE`~~ | ~~4h~~ | ✅ Done — `CdcTriggerMode` enum in config.rs; `rebuild_cdc_triggers()` in api.rs; 0.3.0→0.4.0 upgrade script migrates existing triggers |
| ~~B3~~ | ~~Write-side benchmark matrix (narrow/medium/wide tables × bulk/single DML)~~ | ~~2h~~ | ✅ Done — `bench_stmt_vs_row_cdc_matrix` + `bench_stmt_vs_row_cdc_quick` in e2e_bench_tests.rs; runs via `cargo test -- --ignored bench_stmt_vs_row_cdc_matrix` |

> **Statement-level CDC subtotal: ✅ All done (~14h)**

### Cross-Source Snapshot Consistency (Phase 1)

> **In plain terms:** Imagine a stream table that joins `orders` and
> `customers`. If a single transaction updates both tables, the old scheduler
> could read the new `orders` data but the old `customers` data — a
> half-applied, internally inconsistent snapshot. This fix takes a "freeze
> frame" of the change log at the start of each scheduler tick and only
> processes changes up to that point, so all sources are always read from the
> same moment in time. Zero configuration required.

At start of each scheduler tick, snapshot `pg_current_wal_lsn()` as a
`tick_watermark` and cap all CDC consumption to that LSN. Zero user
configuration — prevents interleaved reads from two sources that were
updated in the same transaction from producing an inconsistent stream table.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~CSS1~~ | ~~LSN tick watermark: snapshot `pg_current_wal_lsn()` per tick; cap frontier advance; log in `pgt_refresh_history`; `pg_trickle.tick_watermark_enabled` GUC (default `on`)~~ | ~~3–4h~~ | ✅ Done |

> **Cross-source consistency subtotal: ✅ All done**

### Ergonomic Hardening

> **In plain terms:** Added helpful warning messages for common mistakes:
> "your WAL level isn't configured for logical replication", "this source
> table has no primary key — duplicate rows may appear", "this change will
> trigger a full re-scan of all source data". Think of these as friendly
> guardrails that explain *why* something might not work as expected.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~ERG-B~~ | ~~Warn at `_PG_init` when `cdc_mode='auto'` but `wal_level != 'logical'` — prevents silent trigger-only operation~~ | ~~30min~~ | ✅ Done |
| ~~ERG-C~~ | ~~Warn at `create_stream_table` when source has no primary key — surfaces keyless duplicate-row risk~~ | ~~1h~~ | ✅ Done (pre-existing in `warn_source_table_properties`) |
| ~~ERG-F~~ | ~~Emit `WARNING` when `alter_stream_table` triggers an implicit full refresh~~ | ~~1h~~ | ✅ Done |

> **Ergonomic hardening subtotal: ✅ All done**

### Code Coverage

> **In plain terms:** Every pull request now automatically reports what
> percentage of the code is exercised by tests, and which specific lines are
> never touched. It's like a map that highlights the unlit corners — helpful
> for spotting blind spots before they become bugs.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~COV~~ | ~~Codecov integration: move token to `with:`, add `codecov.yml` with patch targets for `src/dvm/`, add README badge, verify first upload~~ | ~~1–2h~~ | ✅ Done — reports live at [app.codecov.io/github/grove/pg-trickle](https://app.codecov.io/github/grove/pg-trickle) |

> **v0.4.0 total: ~60–94 hours**

**Exit criteria:**
- [x] `max_concurrent_refreshes` drives real parallel refresh via coordinator + dynamic refresh workers
- [x] Statement-level CDC triggers implemented (B1/B2/B3); benchmark harness in `bench_stmt_vs_row_cdc_matrix`
- [x] LSN tick watermark active by default; no interleaved-source inconsistency in E2E tests
- [x] Codecov badge on README; coverage report uploading
- [x] Extension upgrade path tested (`0.3.0 → 0.4.0`)

---

## v0.5.0 — Row-Level Security & Operational Controls

**Goal:** Harden the security context for stream tables and IVM triggers,
add source-level pause/resume gating for bulk-load coordination, and deliver
small ergonomic improvements.

### Row-Level Security (RLS) Support

> **In plain terms:** Row-level security lets you write policies like "user
> Alice can only see rows where `tenant_id = 'alice'`". Stream tables already
> honour these policies when users query them. What this work fixes is the
> *machinery behind the scenes* — the triggers and refresh functions that
> build the stream table need to see *all* rows regardless of who is running
> them, otherwise they'd produce an incomplete result. This phase hardens
> those internal components so they always have full visibility, while
> end-users still see only their filtered slice.

Stream tables materialize the full result set (like `MATERIALIZED VIEW`). RLS
is applied on the stream table itself for read-side filtering. Phase 1
hardens the security context; Phase 2 adds a tutorial; Phase 3 completes DDL
tracking. Phase 4 (per-role `security_invoker`) is deferred to post-1.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R1 | Document RLS semantics in SQL_REFERENCE.md and FAQ.md | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 | ✅ Done |
| R2 | Disable RLS on change buffer tables (`ALTER TABLE ... DISABLE ROW LEVEL SECURITY`) | 30min | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R2 | ✅ Done |
| R3 | Force superuser context for manual `refresh_stream_table()` (prevent "who refreshed it?" hazard) | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R3 | ✅ Done |
| R4 | Force SECURITY DEFINER on IVM trigger functions (IMMEDIATE mode delta queries must see all rows) | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R4 | ✅ Done |
| R5 | E2E test: RLS on source table does not affect stream table content | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.1 R5 | ✅ Done |
| R6 | Tutorial: RLS on stream tables (enable RLS, per-tenant policies, verify filtering) | 1.5h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R6 | ✅ Done |
| R7 | E2E test: RLS on stream table filters reads per role | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R7 | ✅ Done |
| R8 | E2E test: IMMEDIATE mode + RLS on stream table | 30min | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.2 R8 | ✅ Done |
| R9 | Track ENABLE/DISABLE RLS DDL on source tables (AT_EnableRowSecurity et al.) in hooks.rs | 2h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.3 R9 | ✅ Done |
| R10 | E2E test: ENABLE RLS on source table triggers reinit | 1h | [PLAN_ROW_LEVEL_SECURITY.md](plans/sql/PLAN_ROW_LEVEL_SECURITY.md) §3.3 R10 | ✅ Done |

> **RLS subtotal: ~8–12 hours** (Phase 4 `security_invoker` deferred to post-1.0)

### Bootstrap Source Gating

> **In plain terms:** A pause/resume switch for individual source tables.
> If you're bulk-loading 10 million rows into a source table (a nightly ETL
> import, for example), you can "gate" it first — the scheduler will skip
> refreshing any stream table that reads from it. Once the load is done you
> "ungate" it and a single clean refresh runs. Without gating, the CDC system
> would frantically process millions of intermediate changes during the load,
> most of which get immediately overwritten anyway.

Allow operators to pause CDC consumption for specific source tables (e.g.
during bulk loads or ETL windows) without dropping and recreating stream
tables. The scheduler skips any stream table whose transitive source set
intersects the current gated set.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BOOT-1 | `pgtrickle.pgt_source_gates` catalog table (`source_relid`, `gated`, `gated_at`, `gated_by`) | 30min | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) | ✅ Done |
| BOOT-2 | `gate_source(source TEXT)` SQL function — sets gate, pg_notify scheduler | 1h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) | ✅ Done |
| BOOT-3 | `ungate_source(source TEXT)` + `source_gates()` introspection view | 30min | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) | ✅ Done |
| BOOT-4 | Scheduler integration: load gated-source set per tick; skip and log `SKIP` in `pgt_refresh_history` | 2–3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) | ✅ Done |
| BOOT-5 | E2E tests: single-source gate, coordinated multi-source, partial DAG, bootstrap with `initialize => false` | 3–4h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) | ✅ Done |

> **Bootstrap source gating subtotal: ~7–9 hours**

### Ergonomics & API Polish

> **In plain terms:** A handful of quality-of-life improvements: track when
> someone manually triggered a refresh and log it in the history table; a
> one-row `quick_health` view that tells you at a glance whether the
> extension is healthy (total tables, any errors, any stale tables, scheduler
> running); a `create_stream_table_if_not_exists()` helper so deployment
> scripts don't crash if the table was already created; and `CALL` syntax
> wrappers so the functions feel like native PostgreSQL commands rather than
> extension functions.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ERG-D | Record manual `refresh_stream_table()` calls in `pgt_refresh_history` with `initiated_by='MANUAL'` | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §D | ✅ Done |
| ERG-E | `pgtrickle.quick_health` view — single-row status summary (`total_stream_tables`, `error_tables`, `stale_tables`, `scheduler_running`, `status`) | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §E | ✅ Done |
| COR-2 | `create_stream_table_if_not_exists()` convenience wrapper | 30min | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) §COR-2 | ✅ Done |
| ~~NAT-CALL~~ | ~~`CREATE PROCEDURE` wrappers for all four main SQL functions — enables `CALL pgtrickle.create_stream_table(...)` syntax~~ | ~~1h~~ | Deferred — PostgreSQL does not allow procedures and functions with the same name and argument types |

> **Ergonomics subtotal: ~5–5.5 hours (NAT-CALL deferred)**

### Performance Foundations (Wave 1)

> These quick-win items from [PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) ship
> alongside the RLS and operational work. Read the risk analyses in that document
> before implementing any item.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-3a | MERGE bypass — Append-Only INSERT path: expose `APPEND ONLY` declaration on `CREATE STREAM TABLE`; CDC heuristic fallback (fast-path until first DELETE/UPDATE seen) | 1–2 wk | [PLAN_NEW_STUFF.md §A-3](plans/performance/PLAN_NEW_STUFF.md) | ✅ Done |

> A-4, B-2, and C-4 deferred to v0.6.0 Performance Wave 2 (scope mismatch with the
> RLS/operational-controls theme; correctness risk warrants a dedicated wave).

> **Performance foundations subtotal: ~10–20h (A-3a only)**

> **v0.5.0 total: ~51–97h**

**Exit criteria:**
- [x] RLS semantics documented; change buffers RLS-hardened; IVM triggers SECURITY DEFINER
- [x] RLS on stream table E2E-tested (DIFFERENTIAL + IMMEDIATE)
- [x] `gate_source` / `ungate_source` operational; scheduler skips gated sources correctly
- [x] `quick_health` view and `create_stream_table_if_not_exists` available
- [x] Manual refresh calls recorded in history with `initiated_by='MANUAL'`
- [x] A-3a: Append-Only INSERT path eliminates MERGE for event-sourced stream tables
- [x] Extension upgrade path tested (`0.4.0 → 0.5.0`)

**Status: Released (2026-03-13).**

---

## v0.6.0 — Partitioning, Idempotent DDL, Edge Cases & Circular Dependency Foundation

**Goal:** Validate partitioned source tables, add `create_or_replace_stream_table`
for idempotent deployments (critical for dbt and migration workflows), close all
remaining P0/P1 edge cases and two usability-tier gaps, harden ergonomics and
source gating, expand the dbt integration, fill SQL documentation gaps, and lay
the foundation for circular stream table DAGs.

### Partitioning Support (Source Tables)

> **In plain terms:** PostgreSQL lets you split large tables into smaller
> "partitions" — for example one partition per month for an `orders` table.
> This is a common technique for managing very large datasets. This work
> teaches pg_trickle to track all those partitions as a unit, so adding a
> new monthly partition doesn't silently break stream tables that depend on
> `orders`. It also handles the special case of foreign tables (tables that
> live in another database), restricting them to full-scan refresh since they
> can't be change-tracked the normal way.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~PT1~~ | ~~**Verify partitioned tables work end-to-end.** Create stream tables over RANGE-partitioned source tables, insert/update/delete rows, refresh, and confirm results match — proving that pg_trickle handles partitions correctly out of the box.~~ | 8–12h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §7 |
| ~~PT2~~ | ~~**Detect new partitions automatically.** When someone runs `ALTER TABLE orders ATTACH PARTITION orders_2026_04 ...`, pg_trickle notices and rebuilds affected stream tables so the new partition's data is included. Without this, the new partition would be silently ignored.~~ | 4–8h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.3 |
| ~~PT3~~ | ~~**Make WAL-based change tracking work with partitions.** PostgreSQL's logical replication normally sends changes tagged with the child partition name, not the parent. This configures it to report changes under the parent table name so pg_trickle's WAL decoder can match them correctly.~~ | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.4 |
| ~~PT4~~ | ~~**Handle foreign tables gracefully.** Tables that live in another database (via `postgres_fdw`) can't have triggers or WAL tracking. pg_trickle now detects them and automatically uses full-scan refresh mode instead of failing with a confusing error.~~ | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §6.3 |
| ~~PT5~~ | ~~**Document partitioned table support.** User-facing guide covering which partition types work, what happens when you add/remove partitions, and known caveats.~~ | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §8 |

> **Partitioning subtotal: ~18–32 hours**

### ~~Idempotent DDL (`create_or_replace`)~~ ✅

> **In plain terms:** Right now if you run `create_stream_table()` twice with
> the same name it errors out, and changing the query means
> `drop_stream_table()` followed by `create_stream_table()` — which loses all
> the data in between. `create_or_replace_stream_table()` does the right
> thing automatically: if nothing changed it's a no-op, if only settings
> changed it updates in place, if the query changed it rebuilds. This is the
> same pattern as `CREATE OR REPLACE FUNCTION` in PostgreSQL — and it's
> exactly what the dbt materialization macro needs so every `dbt run` doesn't
> drop and recreate tables from scratch.

`create_or_replace_stream_table()` performs a smart diff: no-op if identical,
in-place alter for config-only changes, schema migration for ADD/DROP column,
full rebuild for incompatible changes. Eliminates the drop-and-recreate
pattern used by the dbt materialization macro.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~COR-1~~ | ~~**The core function.** `create_or_replace_stream_table()` compares the new definition against the existing one and picks the cheapest path: no-op if identical, settings-only update if just config changed, column migration if columns were added/dropped, or full rebuild if the query is fundamentally different. One function call replaces the drop-and-recreate dance.~~ | 4h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| ~~COR-3~~ | ~~**dbt just works.** Updates the `stream_table` dbt materialization macro to call `create_or_replace` instead of dropping and recreating on every `dbt run`. Existing data survives deployments; only genuinely changed stream tables get rebuilt.~~ | 2h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| ~~COR-4~~ | ~~**Upgrade path and documentation.** Upgrade SQL script so existing installations get the new function via `ALTER EXTENSION UPDATE`. SQL Reference and FAQ updated with usage examples.~~ | 2.5h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| ~~COR-5~~ | ~~**Thorough test coverage.** 13 end-to-end tests covering: identical no-op, config-only change, query change with compatible columns, query change with incompatible columns, mode switches, and error cases.~~ | 4h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |

> **Idempotent DDL subtotal: ~12–13 hours**

### Circular Dependency Foundation ✅

> **In plain terms:** Normally stream tables form a one-way chain: A feeds
> B, B feeds C. A circular dependency means A feeds B which feeds A —
> usually a mistake, but occasionally useful for iterative computations like
> graph reachability or recursive aggregations. This lays the groundwork —
> the algorithms, catalog columns, and GUC settings — to eventually allow
> controlled circular stream tables. The actual live execution is completed
> in v0.7.0.

Forms the prerequisite for full SCC-based fixpoint refresh in v0.7.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~CYC-1~~ | ~~**Find cycles in the dependency graph.** Implement Tarjan's algorithm to efficiently detect which stream tables form circular groups. This tells the scheduler "these three stream tables reference each other — they need special handling."~~ | ~2h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 1 |
| ~~CYC-2~~ | ~~**Block unsafe cycles.** Not all queries can safely participate in a cycle — aggregates, EXCEPT, window functions, and NOT EXISTS can't converge to a stable answer when run in a loop. This checker rejects those at creation time with a clear error explaining why.~~ | ~1h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 2 |
| ~~CYC-3~~ | ~~**Track cycles in the catalog.** Add columns to the internal tables that record which cycle group each stream table belongs to and how many iterations the last refresh took. Needed for monitoring and the scheduler logic in v0.7.0.~~ | ~1h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 3 |
| ~~CYC-4~~ | ~~**Safety knobs.** Two new settings: `max_fixpoint_iterations` (default 100) prevents runaway loops, and `allow_circular` (default off) is the master switch — circular dependencies are rejected unless you explicitly opt in.~~ | ~30min | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 4 |

> **Circular dependency foundation subtotal: ~4.5 hours**

### Edge Case Hardening

> **In plain terms:** Six remaining edge cases from the
> [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) catalogue — one data
> correctness issue (P0), three operational-surprise items (P1), and two
> usability gaps (P2). Together they close every open edge case above
> "accepted trade-off" status.

#### P0 — Data Correctness

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~EC-19~~ ✅ | ~~**Prevent silent data corruption with WAL + keyless tables.** If you use WAL-based change tracking on a table without a primary key, PostgreSQL needs `REPLICA IDENTITY FULL` to send complete row data. Without it, deltas are silently incomplete. This rejects the combination at creation time with a clear error instead of producing wrong results.~~ | 0.5 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-19 |

#### P1 — Operational Safety

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~EC-16~~ ✅ | ~~**Detect when someone silently changes a function your query uses.** If a stream table's query calls `calculate_discount()` and someone does `CREATE OR REPLACE FUNCTION calculate_discount(...)` with new logic, the stream table's cached computation plan becomes stale. This checks function body hashes on each refresh and triggers a rebuild when a change is detected.~~ | 2 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-16 |
| ~~EC-18~~ ✅ | ~~**Explain why WAL mode isn't activating.** When `cdc_mode = 'auto'`, pg_trickle is supposed to upgrade from trigger-based to WAL-based change tracking when possible. If it stays stuck on triggers (e.g. because `wal_level` isn't set to `logical`), there's no feedback. This adds a periodic log message explaining the reason and surfaces it in the `health_check()` output.~~ | 1 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-18 |
| ~~EC-34~~ ✅ | ~~**Recover gracefully after restoring from backup.** When you restore a PostgreSQL server from `pg_basebackup`, replication slots are lost. pg_trickle's WAL decoder would fail trying to read from a slot that no longer exists. This detects the missing slot, automatically falls back to trigger-based tracking, and logs a WARNING so you know what happened.~~ | 1 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-34 |

#### P2 — Usability Gaps

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~EC-03~~ ✅ | ~~**Support window functions inside expressions.** Queries like `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN 'first' ELSE 'other' END` are currently rejected because the incremental engine can't handle a window function nested inside a CASE. This automatically extracts the window function into a preliminary step and rewrites the outer query to reference the precomputed result — so the query pattern just works.~~ | 3–5 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-03 |
| ~~EC-32~~ ✅ | ~~**Support `ALL (subquery)` comparisons.** Queries like `WHERE price > ALL (SELECT price FROM competitors)` (meaning "greater than every row in the subquery") are currently rejected in incremental mode. This rewrites them into an equivalent form the engine can handle, removing a Known Limitation from the changelog.~~ | 2–3 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-32 |

> **Edge case hardening subtotal: ~9.5–13.5 days**

### ~~Ergonomics Follow-Up~~ ✅

> **In plain terms:** Several test gaps and a documentation item were left
> over from the v0.5.0 ergonomics work. These are all small E2E tests that
> confirm existing features actually produce the warnings and errors they're
> supposed to — catching regressions before users hit them. The changelog
> entry documents breaking behavioural changes (the default schedule changed
> from a fixed "every 1 minute" to an auto-calculated interval, and `NULL`
> schedule input is now rejected).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~ERG-T1~~ ✅ | ~~**Test the smart schedule default.** Verify that passing `'calculated'` as a schedule works (pg_trickle picks an interval based on table size) and that passing `NULL` gives a clear error instead of silently breaking. Catches regressions in the schedule parser.~~ | 4h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ~~ERG-T2~~ ✅ | ~~**Test that removed settings stay removed.** The `diamond_consistency` GUC was removed in v0.4.0. Verify that `SHOW pg_trickle.diamond_consistency` returns an error — not a stale value from a previous installation that confuses users.~~ | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ~~ERG-T3~~ ✅ | ~~**Test the "heads up, this will do a full refresh" warning.** When you change a stream table's query via `alter_stream_table(query => ...)`, it may trigger an expensive full re-scan. Verify the WARNING appears so users aren't surprised by a sudden spike in load.~~ | 3h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ~~ERG-T4~~ ✅ | ~~**Test the WAL configuration warning.** When `cdc_mode = 'auto'` but PostgreSQL's `wal_level` isn't set to `logical`, pg_trickle can't use WAL-based tracking and silently falls back to triggers. Verify the startup WARNING appears so operators know they need to change `wal_level`.~~ | 3h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ~~ERG-T5~~ ✅ | ~~**Document breaking changes in the changelog.** In v0.4.0 the default schedule changed from "every 1 minute" to auto-calculated, and `NULL` schedule input started being rejected. These behavioural changes need explicit CHANGELOG entries so upgrading users aren't caught off guard.~~ | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |

> **Ergonomics follow-up subtotal: ~14 hours**

### ~~Bootstrap Source Gating Follow-Up~~ ✅

> **In plain terms:** Source gating (pause/resume for bulk loads) shipped in
> v0.5.0 with the core API and scheduler integration. This follow-up adds
> robustness tests for edge cases that real-world ETL pipelines will hit:
> What happens if you gate a source twice? What if you re-gate it after
> ungating? It also adds a dedicated introspection function that shows the
> full gate lifecycle (when gated, who gated it, how long it's been gated),
> and documentation showing common ETL coordination patterns like
> "gate → bulk load → ungate → single clean refresh."

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~BOOT-F1~~ | ~~**Calling gate twice is safe.** Verify that calling `gate_source('orders')` when `orders` is already gated is a harmless no-op — not an error. Important for ETL scripts that may retry on failure.~~ | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| ~~BOOT-F2~~ | ~~**Gate → ungate → gate again works correctly.** Verify the full lifecycle: gate a source (scheduler skips it), ungate it (scheduler resumes), gate it again (scheduler skips again). Proves the mechanism is reusable across multiple load cycles.~~ | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| ~~BOOT-F3~~ | ~~**See your gates at a glance.** A new `bootstrap_gate_status()` function that shows which sources are gated, when they were gated, who gated them, and how long they've been paused. Useful for debugging when the scheduler seems to be "doing nothing" — it might just be waiting for a gate.~~ | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| ~~BOOT-F4~~ | ~~**Cookbook for common ETL patterns.** Documentation with step-by-step recipes: gating a single source during a bulk load, coordinating multiple source loads that must finish together, gating only part of a stream table DAG, and the classic "nightly batch → gate → load → ungate → single clean refresh" workflow.~~ | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |

> **Bootstrap gating follow-up subtotal: ~12 hours**

### ~~dbt Integration Enhancements~~ ✅

> **In plain terms:** The dbt macro package (`dbt-pgtrickle`) shipped in
> v0.4.0 with the core `stream_table` materialization. This adds three
> improvements: a `stream_table_status` macro that lets dbt models query
> health information (stale? erroring? how many refreshes?) so you can build
> dbt tests that fail when a stream table is unhealthy; a bulk
> `refresh_all_stream_tables` operation for CI pipelines that need everything
> fresh before running tests; and expanded integration tests covering the
> `alter_stream_table` flow (which gets more important once
> `create_or_replace` lands in the same release).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DBT-1~~ | ~~**Check stream table health from dbt.** A new `stream_table_status()` macro that returns whether a stream table is healthy, stale, or erroring — so you can write dbt tests like "fail if the orders summary hasn't refreshed in the last 5 minutes." Makes pg_trickle a first-class citizen in dbt's testing framework.~~ | 3h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |
| ~~DBT-2~~ | ~~**Refresh everything in one command.** A `dbt run-operation refresh_all_stream_tables` command that refreshes all stream tables in the correct dependency order. Designed for CI pipelines: run it after `dbt run` and before `dbt test` to make sure all materialized data is current.~~ | 2h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |
| ~~DBT-3~~ | ~~**Test the dbt ↔ alter flow.** Integration tests that verify query changes, config changes, and mode switches all work correctly when made through dbt's `stream_table` materialization. Especially important now that `create_or_replace` is landing in the same release.~~ | 3h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |

> **dbt integration subtotal: ~8 hours**

### ~~SQL Documentation Gaps~~ ✅

> **In plain terms:** Once EC-03 (window functions in expressions) and EC-32
> (`ALL (subquery)`) are implemented in this release, the documentation needs
> to explain the new patterns with examples. The foreign table polling CDC
> feature (shipped in v0.2.2) also needs a worked example showing common
> setups like `postgres_fdw` source tables with periodic polling.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DOC-1~~ | ~~**Show users how ALL-subqueries work.** Once EC-32 lands, add a SQL Reference section explaining `WHERE price > ALL (SELECT ...)`, how pg_trickle rewrites it internally, and a complete worked example with sample data and expected output.~~ | 2h | [GAP_SQL_OVERVIEW.md](plans/sql/GAP_SQL_OVERVIEW.md) |
| ~~DOC-2~~ | ~~**Show the window-in-expression pattern.** Once EC-03 lands, add a before/after example to the SQL Reference: "Here's your original query with `CASE WHEN ROW_NUMBER() ...`, and here's what pg_trickle does under the hood to make it work incrementally."~~ | 2h | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-03 |
| ~~DOC-3~~ | ~~**Walkthrough for foreign table sources.** A step-by-step recipe showing how to create a `postgres_fdw` foreign table, use it as a stream table source with polling-based change detection, and what to expect in terms of refresh behaviour. This feature shipped in v0.2.2 but was never properly documented with an example.~~ | 1h | Existing feature (v0.2.2) |

> **SQL documentation subtotal: ~5 hours**

> **v0.6.0 total: ~77–92h**

**Exit criteria:**
- [x] Partitioned source tables E2E-tested; ATTACH PARTITION detected
- [x] WAL mode works with `publish_via_partition_root = true`
- [x] `create_or_replace_stream_table` deployed; dbt macro updated
- [x] SCC algorithm in place; monotonicity checker rejects non-monotone cycles
- [x] WAL + keyless without REPLICA IDENTITY FULL rejected at creation (EC-19)
- [x] `ALTER FUNCTION` body changes detected via `pg_proc` hash polling (EC-16)
- [x] Stuck `auto` CDC mode surfaces explanation in logs and health check (EC-18)
- [x] Missing WAL slot after restore auto-detected with TRIGGER fallback (EC-34)
- [x] Window functions in expressions supported via subquery-lift rewrite (EC-03)
- [x] `ALL (subquery)` rewritten to NULL-safe anti-join (EC-32)
- [x] Ergonomics E2E tests for calculated schedule, warnings, and removed GUCs pass
- [x] `gate_source()` idempotency and re-gating tested; `bootstrap_gate_status()` available
- [x] dbt `stream_table_status()` and `refresh_all_stream_tables` macros shipped
- [x] SQL Reference updated for EC-03, EC-32, and foreign table polling patterns
- [x] Extension upgrade path tested (`0.5.0 → 0.6.0`)

**Status: Released (2026-03-14).**

---

## v0.7.0 — Performance, Watermarks, Circular DAG Execution, Observability & Infrastructure

**Status: Released (2026-03-16).**

**Goal:** Land Part 9 performance improvements (parallel refresh
scheduling, MERGE strategy optimization, advanced benchmarks), add
user-injected temporal watermark gating for batch-ETL coordination,
complete the fixpoint scheduler for circular stream table DAGs, ship
ready-made Prometheus/Grafana monitoring, and prepare the 1.0 packaging
and deployment infrastructure.

### Watermark Gating

> **In plain terms:** A scheduling control for ETL pipelines where multiple
> source tables are populated by separate jobs that finish at different
> times. For example, `orders` might be loaded by a job that finishes at
> 02:00 and `products` by one that finishes at 03:00. Without watermarks,
> the scheduler might refresh a stream table that joins the two at 02:30,
> producing a half-complete result. Watermarks let each ETL job declare "I'm
> done up to timestamp X", and the scheduler waits until all sources are
> caught up within a configurable tolerance before proceeding.

Let producers signal their progress so the scheduler only refreshes stream
tables when all contributing sources are aligned within a configurable
tolerance. The primary use case is nightly batch ETL pipelines where multiple
source tables are populated on different schedules.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~WM-1~~ | ~~Catalog: `pgt_watermarks` table (`source_relid`, `current_watermark`, `updated_at`, `wal_lsn_at_advance`); `pgt_watermark_groups` table (`group_name`, `sources`, `tolerance`)~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| ~~WM-2~~ | ~~`advance_watermark(source, watermark)` — monotonicity check, store LSN alongside watermark, lightweight scheduler signal~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| ~~WM-3~~ | ~~`create_watermark_group(name, sources[], tolerance)` / `drop_watermark_group()`~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| ~~WM-4~~ | ~~Scheduler pre-check: evaluate watermark alignment predicate; skip + log `SKIP(watermark_misaligned)` if not aligned~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| ~~WM-5~~ | ~~`watermarks()`, `watermark_groups()`, `watermark_status()` introspection functions~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| ~~WM-6~~ | ~~E2E tests: nightly ETL, micro-batch tolerance, multiple pipelines, mixed external+internal sources~~ | ✅ Done | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |

> **Watermark gating: ✅ Complete**

### Circular Dependencies — Scheduler Integration

> **In plain terms:** Completes the circular DAG work started in v0.6.0.
> When stream tables reference each other in a cycle (A → B → A), the
> scheduler now runs them repeatedly until the result stabilises — no more
> changes flowing through the cycle. This is called "fixpoint iteration",
> like solving a system of equations by re-running it until the numbers stop
> moving. If it doesn't converge within a configurable number of rounds
> (default 100) it surfaces an error rather than looping forever.

Completes the SCC foundation from v0.6.0 with a working fixpoint iteration
loop. Stream tables in a monotone cycle are refreshed repeatedly until
convergence (zero net change) or `max_fixpoint_iterations` is exceeded.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~CYC-5~~ | ~~Scheduler fixpoint iteration: `iterate_to_fixpoint()`, convergence detection from `(rows_inserted, rows_deleted)`, non-convergence → `ERROR` status~~ | ✅ Done | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 5 |
| ~~CYC-6~~ | ~~Creation-time validation: allow monotone cycles when `allow_circular=true`; assign `scc_id`; recompute SCCs on `drop_stream_table`~~ | ✅ Done | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 6 |
| ~~CYC-7~~ | ~~Monitoring: `scc_id` + `last_fixpoint_iterations` in views; `pgtrickle.pgt_scc_status()` function~~ | ✅ Done | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 7 |
| ~~CYC-8~~ | ~~Documentation + E2E tests (`e2e_circular_tests.rs`): 6 scenarios (monotone cycle, non-monotone reject, convergence, non-convergence→ERROR, drop breaks cycle, `allow_circular=false` default)~~ | ✅ Done | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 8 |

> **Circular dependencies subtotal: ~19 hours**

### Last Differential Mode Gaps

> **In plain terms:** Three query patterns that previously fell back to `FULL`
> refresh in `AUTO` mode — or hard-errored in explicit `DIFFERENTIAL` mode
> — despite the DVM engine having the infrastructure to handle them.
> All three gaps are now closed.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DG-1~~ | ~~**User-Defined Aggregates (UDAs).** PostGIS (`ST_Union`, `ST_Collect`), pgvector vector averages, and any `CREATE AGGREGATE` function are rejected. Fix: classify unknown aggregates as `AggFunc::UserDefined` and route them through the existing group-rescan strategy — no new delta math required.~~ | ✅ Done | [PLAN_LAST_DIFFERENTIAL_GAPS.md](plans/sql/PLAN_LAST_DIFFERENTIAL_GAPS.md) §G1 |
| ~~DG-2~~ | ~~**Window functions nested in expressions.** `RANK() OVER (...) + 1`, `CASE WHEN ROW_NUMBER() OVER (...) <= 10`, `COALESCE(LAG(v) OVER (...), 0)` etc. are rejected.~~ | ✅ Done (v0.6.0) | [PLAN_LAST_DIFFERENTIAL_GAPS.md](plans/sql/PLAN_LAST_DIFFERENTIAL_GAPS.md) §G2 |
| ~~DG-3~~ | ~~**Sublinks in deeply nested OR.** The two-stage rewrite pipeline handles flat `EXISTS(...) OR …` and `AND(EXISTS OR …)` but gives up on multiple OR+sublink conjuncts. Fix: expand all OR+sublink conjuncts in AND to a cartesian product of UNION branches with a 16-branch explosion guard.~~ | ✅ Done | [PLAN_LAST_DIFFERENTIAL_GAPS.md](plans/sql/PLAN_LAST_DIFFERENTIAL_GAPS.md) §G3 |

> **Last differential gaps: ✅ Complete**

### Pre-1.0 Infrastructure Prep

> **In plain terms:** Three preparatory tasks that make the eventual 1.0
> release smoother. A draft Docker Hub image workflow (tests the build but
> doesn't publish yet); a PGXN metadata file so the extension can eventually
> be installed with `pgxn install pg_trickle`; and a basic CNPG integration
> test that verifies the extension image loads correctly in a CloudNativePG
> cluster. None of these ship user-facing features — they're CI and
> packaging scaffolding.

| Item | Description | Effort | Ref |
|------|-------------|--------|---------|
| ~~INFRA-1~~ | ~~**Prove the Docker image builds.** Set up a CI workflow that builds the official Docker Hub image (PostgreSQL 18 + pg_trickle pre-installed), runs a smoke test (create extension, create a stream table, refresh it), but doesn't publish anywhere yet. When 1.0 arrives, publishing is just flipping a switch.~~ | 5h | ✅ Done |
| ~~INFRA-2~~ | ~~**Publish an early PGXN testing release.** Draft `META.json` and upload a `release_status: "testing"` package to PGXN so `pgxn install pg_trickle` works for early adopters now. PGXN explicitly supports pre-stable releases; this gets real-world install testing and establishes registry presence before 1.0. At 1.0 the only change is flipping `release_status` to `"stable"`.~~ | 2–3h | ✅ Done |
| ~~INFRA-3~~ | ~~**Verify Kubernetes deployment works.** A CI smoke test that deploys the pg_trickle extension image into a CloudNativePG (CNPG) Kubernetes cluster, creates a stream table, and confirms a refresh cycle completes. Catches packaging and compatibility issues before they reach Kubernetes users.~~ | 4h | ✅ Done |

> **Pre-1.0 infrastructure prep: ✅ Complete**

### Performance — Regression Fixes & Benchmark Infrastructure (Part 9 S1–S2) ✅ Done

> Fixes Criterion benchmark regressions identified in Part 9 and ships five
> benchmark infrastructure improvements to support data-driven performance
> decisions.

| Item | Description | Status |
|------|-------------|--------|
| A-3 | Fix `prefixed_col_list/20` +34% regression — eliminate intermediate `Vec` allocation | ✅ Done |
| A-4 | Fix `lsn_gt` +22% regression — use `split_once` instead of `split().collect()` | ✅ Done |
| I-1c | `just bench-docker` target for running Criterion inside Docker builder image | ✅ Done |
| I-2 | Per-cycle `[BENCH_CYCLE]` CSV output in E2E benchmarks for external analysis | ✅ Done |
| I-3 | EXPLAIN ANALYZE capture mode (`PGS_BENCH_EXPLAIN=true`) for delta query plans | ✅ Done |
| I-6 | 1M-row benchmark tier (`bench_*_1m_*` + `bench_large_matrix`) | ✅ Done |
| I-8 | Criterion noise reduction (`sample_size(200)`, `measurement_time(10s)`) | ✅ Done |

### Performance — Parallel Refresh, MERGE Optimization & Advanced Benchmarks (Part 9 S4–S6) ✅ Done

> DAG level-parallel scheduling, improved MERGE strategy selection (xxh64
> hashing, aggregate saturation bypass, cost-based threshold), and expanded
> benchmark suite (JSON comparison, concurrent writers, window/lateral/CTE).

| Item | Description | Status |
|------|-------------|--------|
| C-1 | DAG level extraction (`topological_levels()` on `StDag` and `ExecutionUnitDag`) | ✅ Done |
| C-2 | Level-parallel dispatch (existing `parallel_dispatch_tick` infrastructure sufficient) | ✅ Done |
| C-3 | Result communication (existing `SchedulerJob` + `pgt_refresh_history` sufficient) | ✅ Done |
| D-1 | xxh64 hash-based change detection for wide tables (≥50 cols) | ✅ Done |
| D-2 | Aggregate saturation FULL bypass (changes ≥ groups → FULL) | ✅ Done |
| D-3 | Cost-based strategy selection from `pgt_refresh_history` data | ✅ Done |
| I-4 | Cross-run comparison tool (`just bench-compare`, JSON output) | ✅ Done |
| I-5 | Concurrent writer benchmarks (1/2/4/8 writers) | ✅ Done |
| I-7 | Window / lateral / CTE / UNION ALL operator benchmarks | ✅ Done |

> **v0.7.0 total: ~59–62h**

**Exit criteria:**
- [x] Part 9 performance: DAG levels, xxh64 hashing, aggregate saturation bypass, cost-based threshold, advanced benchmarks
- [x] `advance_watermark` + scheduler gating operational; ETL E2E tests pass
- [x] Monotone circular DAGs converge to fixpoint; non-convergence surfaces as `ERROR`
- [x] UDAs, nested window expressions, and deeply nested OR+sublinks supported in DIFFERENTIAL mode
- [x] Docker Hub image CI workflow builds and smoke-tests successfully
- [x] PGXN `testing` release uploaded; `pgxn install pg_trickle` works
- [x] CNPG integration smoke test passes in CI
- [x] Extension upgrade path tested (`0.6.0 → 0.7.0`)

---

## v0.8.0 — pg_dump Support & Test Hardening

**Status:** Released

**Goal:** Complete the pg_dump round-trip story so stream tables survive
`pg_dump`/`pg_restore` cycles, and comprehensively harden the 
E2E test suites with multiset invariants to mathematically enforce DVM correctness.

### pg_dump / pg_restore Support

> **In plain terms:** `pg_dump` is the standard PostgreSQL backup tool.
> Without this, a dump of a database containing stream tables may not
> capture them correctly — and restoring from that dump would require
> manually recreating them by hand. This teaches `pg_dump` to emit valid
> SQL for every stream table, and adds logic to automatically re-link
> orphaned catalog entries when restoring an extension from a backup.

Complete the native DDL story: teach pg_dump to emit `CREATE MATERIALIZED VIEW
… WITH (pgtrickle.stream = true)` for stream tables and add an event trigger
that re-links orphaned catalog entries on extension restore.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| NAT-DUMP | `generate_dump()` + `restore_stream_tables()` companion functions (done); event trigger on extension load for orphaned catalog entries | 3–4d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §pg_dump |
| NAT-TEST | E2E tests: pg_dump round-trip, restore from backup, orphaned-entry recovery | 2–3d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §pg_dump |

> **pg_dump support subtotal: ~5–7 days**

### Test Suite Evaluation & Hardening

> **In plain terms:** Replacing legacy, row-count-based assertions with comprehensive, order-independent multiset evaluations (`assert_st_matches_query`) across all testing tiers. This mathematical invariant proving guarantees differential dataflow correctness under highly chaotic multiset interleavings and edge cases.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TE1 | **Unit Test Hardening:** Full multiset equality testing for pure-Rust DVM operators | Done | [PLAN_EVALS_UNIT](plans/testing/PLAN_TEST_EVALS_UNIT.md) |
| TE2 | **Light E2E Migration:** Expand speed-optimized E2E pipeline with rigorous symmetric difference checks | Done | [PLAN_EVALS_LIGHT_E2E](plans/testing/PLAN_TEST_EVALS_LIGHT_E2E.md) |
| TE3 | **Integration Concurrency:** Prove complex orchestration correctness under transaction delays | Done | [PLAN_EVALS_INTEGRATION](plans/testing/PLAN_TEST_EVALS_INTEGRATION.md) |
| TE4 | **Full E2E Hardening:** Validate cross-boundary, multi-DAG cascades, partition handling, and upgrade paths | Done | [PLAN_EVALS_FULL_E2E](plans/testing/PLAN_TEST_EVALS_FULL_E2E.md) |
| TE5 | **TPC-H Smoke Test:** Stateful invariant evaluations for heavily randomized DML loads over large matrices | Done | [PLAN_EVALS_TPCH](plans/testing/PLAN_TEST_EVALS_TPCH.md) |
| TE6 | **Property-Based Invariants:** Chaotic property testing pipelines for topological boundaries and cyclic executions | Done | [PLAN_PROPERTY_BASED_INVARIANTS](plans/testing/PLAN_TEST_PROPERTY_BASED_INVARIANTS.md) |
| TE7 | **cargo-nextest Migration:** Move test suite execution to cargo-nextest to aggressively parallelize and isolate tests, solving wall-clock execution regressions | 1–2d | [PLAN_CARGO_NEXTEST](plans/testing/PLAN_CARGO_NEXTEST.md) |

> **Test evaluation subtotal: ~11-14 days (Mostly Completed)**

> **v0.8.0 total: ~16–21 days**

**Exit criteria:**
- [x] Test infrastructure hardened with exact mathematical multiset validation
- [ ] Test harness migrated to `cargo-nextest` to fix speed and CI flake regressions
- [x] pg_dump round-trip produces valid, restorable SQL for stream tables *(Done)*
- [ ] Extension upgrade path tested (`0.7.0 → 0.8.0`)

---

## v0.9.0 — Incremental Aggregate Maintenance

**Status: Released (2026-03-20).**

**Goal:** Implement algebraic incremental maintenance for decomposable aggregates
(COUNT, SUM, AVG, MIN, MAX, STDDEV), reducing per-group refresh from O(group_size)
to O(1) for the common case. This is the highest-potential-payoff item in the
performance plan — benchmarks show aggregate scenarios going from 2.5 ms to sub-1 ms
per group.

### Critical Bug Fixes

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| G-1 | **`panic!()` in SQL-callable `source_gates()` and `watermarks()` functions.** Both functions reach `panic!()` on any SPI error, crashing the PostgreSQL backend process. AGENTS.md explicitly forbids `panic!()` in code reachable from SQL. Replace both `.unwrap_or_else(\|e\| panic!(…))` calls with `pgrx::error!(…)` so any SPI failure surfaces as a PostgreSQL `ERROR` instead. | ~1h | ✅ Done | [src/api.rs](src/api.rs) |

> **Critical bug fixes subtotal: ~1 hour**

### Algebraic Aggregate Shortcuts (B-1)

> **In plain terms:** When only one row changes in a group of 100,000, today
> pg_trickle re-scans all 100,000 rows to recompute the aggregate. Algebraic
> maintenance keeps running totals: `new_sum = old_sum + Δsum`, `new_count =
> old_count + Δcount`. Only MIN/MAX needs a rescan — and only when the deleted
> value *was* the current minimum or maximum.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| B1-1 | Algebraic rules: COUNT, SUM *(already algebraic)*, AVG *(done — aux cols)*, STDDEV/VAR *(done — sum-of-squares decomposition)*, MIN/MAX with rescan guard *(already implemented)* | 3–4 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-2 | Auxiliary column management (`__pgt_aux_sum_*`, `__pgt_aux_count_*`, `__pgt_aux_sum2_*` — done); hidden via `__pgt_*` naming convention (existing `NOT LIKE '__pgt_%'` filter) | 1–2 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-3 | Migration story for existing aggregate stream tables; periodic full-group recomputation to reset floating-point drift | 1 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-4 | Fallback to full-group recomputation for non-decomposable aggregates (`mode`, percentile, `string_agg` with ordering) | 1 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-5 | Property-based tests: MIN/MAX boundary case (deleting the exact current min or max value must trigger rescan) | 1 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |

#### Implementation Progress

**Completed:**

- **AVG algebraic maintenance (B1-1):** AVG no longer triggers full
  group-rescan. Classified as `is_algebraic_via_aux()` and tracked via
  `__pgt_aux_sum_*` / `__pgt_aux_count_*` columns. The merge expression
  computes `(old_sum + ins - del) / NULLIF(old_count + ins - del, 0)`.

- **STDDEV/VAR algebraic maintenance (B1-1):** `STDDEV_POP`, `STDDEV_SAMP`,
  `VAR_POP`, and `VAR_SAMP` are now algebraic using sum-of-squares
  decomposition. Auxiliary columns: `__pgt_aux_sum_*` (running SUM),
  `__pgt_aux_sum2_*` (running SUM(x²)), `__pgt_aux_count_*`.
  Merge formulas:
  - `VAR_POP = GREATEST(0, (n·sum2 − sum²) / n²)`
  - `VAR_SAMP = GREATEST(0, (n·sum2 − sum²) / (n·(n−1)))`
  - `STDDEV_POP = SQRT(VAR_POP)`, `STDDEV_SAMP = SQRT(VAR_SAMP)`
  Null guards match PostgreSQL semantics (NULL when count ≤ threshold).

- **Auxiliary column infrastructure (B1-2):** `create_stream_table()` and
  `alter_stream_table()` detect AVG/STDDEV/VAR aggregates and automatically
  add `NUMERIC` sum/sum2 and `BIGINT` count columns. Full refresh and
  initialization paths inject `SUM(arg)`, `COUNT(arg)`, and `SUM(arg*arg)`.
  All `__pgt_aux_*` columns are automatically hidden by the existing
  `NOT LIKE '__pgt_%'` convention used throughout the codebase.

- **Non-decomposable fallback (B1-4):** Already existed as the group-rescan
  strategy — any aggregate not classified as algebraic or algebraic-via-aux
  falls back to full group recomputation.

- **Property-based tests (B1-5):** Seven proptest tests verify:
  (a) MIN merge uses `LEAST`, MAX merge uses `GREATEST`;
  (b) deleting the exact current extremum triggers rescan;
  (c) delta expressions use matching aggregate functions;
  (d) AVG is classified as algebraic-via-aux (not group-rescan);
  (e) STDDEV/VAR use sum-of-squares algebraic path with GREATEST guard;
  (f) STDDEV wraps in SQRT, VAR does not;
  (g) DISTINCT STDDEV falls back (not algebraic).

- **Migration story (B1-3):** `ALTER QUERY` transition seamlessly. Handled by
  extending `migrate_aux_columns` to execute `ALTER TABLE ADD COLUMN` or
  `DROP COLUMN` exactly matching runtime changes in the `new_avg_aux` or 
  `new_sum2_aux` definitions.

- **Floating-point drift reset (B1-3):** Implemented global GUC 
  `pg_trickle.algebraic_drift_reset_cycles` (0=disabled) that counts
  differential refresh attempts in scheduler memory per-stream-table. When
  the threshold fires, action degrades to `RefreshAction::Reinitialize`.

- **E2E integration tests:** Tested via multi-cycle inserts, updates, and deletes
  checking proper handling without regression (added specifically for STDDEV/VAR).

**Remaining work:**

- **Extension upgrade path (`0.8.0 → 0.9.0`):** Upgrade SQL stub created. Left as a final pre-release checklist item to generate the final `sql/archive/pg_trickle--0.9.0.sql` with `cargo pgrx package` once all CI checks pass.

- **F15 — Selective CDC Column Capture:** ✅ Complete. Column-selection pipeline, monitoring exposure via `check_cdc_health().selective_capture`, and 3 E2E integration tests done.

> ⚠️ Critical: the MIN/MAX maintenance rule is directionally tricky. The correct
> condition for triggering a rescan is: deleted value **equals** the current min/max
> (not when it differs). Getting this backwards silently produces stale aggregates
> on the most common OLTP delete pattern. See the corrected table and risk analysis
> in PLAN_NEW_STUFF.md §B-1.

> **Retraction consideration (B-1):** Keep in v0.9.0, but item B1-5 (property-based
> tests covering the MIN/MAX boundary case) is a **hard prerequisite** for B1-1, not
> optional follow-on work. The MIN/MAX rule was stated backwards in the original spec;
> the corrected rule is now in PLAN_NEW_STUFF.md. Do not merge any MIN/MAX algebraic
> path until property-based tests confirm: (a) deleting the exact current min triggers
> a rescan and (b) deleting a non-min value does not. Floating-point drift reset
> (B1-3) is also required before enabling persistent auxiliary columns.
>
> ✅ **B1-5 hard prerequisite satisfied.** Property-based tests now cover both
> conditions — see `prop_min_max_rescan_guard_direction` in `tests/property_tests.rs`.

> **Algebraic aggregates subtotal: ~7–9 weeks**

### Advanced SQL Syntax & DVM Capabilities (B-2)

These represent expansions of the DVM engine to handle richer SQL constructs and improve runtime execution consistency.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| B2-1 | **LIMIT / OFFSET / ORDER BY.** Top-K queries evaluated directly within the DVM engine. | 2–3 wk | ✅ Done | [PLAN_ORDER_BY_LIMIT_OFFSET.md](plans/sql/PLAN_ORDER_BY_LIMIT_OFFSET.md) |
| B2-2 | **LATERAL Joins.** Expanding the parser and DVM diff engine to handle LATERAL subqueries. | 2 wk | ✅ Done | [PLAN_LATERAL_JOINS.md](plans/sql/PLAN_LATERAL_JOINS.md) |
| B2-3 | **View Inlining.** Allow stream tables to query standard PostgreSQL views natively. | 1-2 wk | ✅ Done | [PLAN_VIEW_INLINING.md](plans/sql/PLAN_VIEW_INLINING.md) |
| B2-4 | **Synchronous / Transactional IVM.** Evaluating DVM diffs synchronously in the same transaction as the DML. | 3 wk | ✅ Done | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |
| B2-5 | **Cross-Source Snapshot Consistency.** Improving engine consistency models when joining multiple tables. | 2 wk | ✅ Done | [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](plans/sql/PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) |
| B2-6 | **Non-Determinism Guarding.** Better handling or rejection of non-deterministic functions (`random()`, `now()`). | 1 wk | ✅ Done | [PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) |

### Multi-Table Delta Batching (B-3)

> **In plain terms:** When a join query has three source tables and all three
> change in the same cycle, today pg_trickle makes three separate passes through
> the source tables. B-3 merges those passes into one and prunes UNION ALL
> branches for sources with no changes.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| B3-1 | Intra-query delta-branch pruning: skip UNION ALL branch entirely when a source has zero changes in this cycle | 1–2 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |
| B3-2 | Merged-delta generation: weight aggregation (`GROUP BY __pgt_row_id, SUM(weight)`) for cross-source deduplication; remove zero-weight rows | 3–4 wk | ✅ Done (v0.10.0) | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |
| B3-3 | Property-based correctness tests for simultaneous multi-source changes; diamond-flow scenarios | 1–2 wk | ✅ Done (v0.10.0) | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |

> ✅ B3-2 correctly uses weight aggregation (`GROUP BY __pgt_row_id, SUM(weight)`) instead
> of `DISTINCT ON`. B3-3 property-based tests (6 diamond-flow scenarios) verify correctness.

> **Multi-source delta batching subtotal: ~5–8 weeks**

### Phase 7 Gap Resolutions (DVM Correctness, Syntax & Testing)

These items pull in the remaining correctness edge cases and syntax expansions identified in the Phase 7 SQL Gap Analysis, along with completing exhaustive differential E2E test maturation.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|------- |---- |
| G1.1 | **JOIN Key Column Changes.** Handle updates that simultaneously modify a JOIN key and right-side tracked columns. | 3-5d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| G1.2 | **Window Function Partition Drift.** Explicit tracking for updates that cause rows to cross `PARTITION BY` ranges. | 4-6d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| G1.5/G7.1 | **Keyless Table Duplicate Identity.** Resolve `__pgt_row_id` collisions for non-PK tables with exact duplicate rows. | 3-5d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| G5.6 | **Range Aggregates.** Support and differentiate `RANGE_AGG` and `RANGE_INTERSECT_AGG`. | 1-2d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| G5.3 | **XML Expression Parsing.** Native DVM handling for `T_XmlExpr` syntax trees. | 1-2d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| G5.5 | **NATURAL JOIN Drift Tracking.** DVM tracking of schema shifts in `NATURAL JOIN` between refreshes. | 2-3d | ✅ Done | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) |
| F15 | **Selective CDC Column Capture.** Limit row I/O by only tracking columns referenced in query lineage. | 1-2 wk | ✅ Done | [GAP_SQL_PHASE_6.md](plans/sql/GAP_SQL_PHASE_6.md) |
| F40 | **Extension Upgrade Migrations.** Robust versioned SQL schema migrations. | 1-2 wk | ✅ Done | [REPORT_DB_SCHEMA_STABILITY.md](plans/sql/REPORT_DB_SCHEMA_STABILITY.md) |

> **Phase 7 Gaps subtotal: ~5-7 weeks**

### Additional Query Engine Improvements

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| A1 | Circular dependency support (SCC fixpoint iteration) | ~40h | ✅ Done | [CIRCULAR_REFERENCES.md](plans/sql/CIRCULAR_REFERENCES.md) |
| A7 | Skip-unchanged-column scanning in delta SQL (requires column-usage demand-propagation pass in DVM parser) | ~1–2d | ✅ Done | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 4 §3.4 |
| EC-03 | Window-in-expression DIFFERENTIAL fallback warning: emit a `WARNING` (and eventually an `INFO` hint) when a stream table with `CASE WHEN window_fn() OVER (...) ...` silently falls back from DIFFERENTIAL to FULL refresh mode; currently fails at runtime with `column st.* does not exist` — no user-visible signal exists | ~1d | ✅ Done | [PLAN_EDGE_CASES.md §EC-03](plans/PLAN_EDGE_CASES.md) |
| A8 | `pgt_refresh_groups` SQL API: companion functions (`pgtrickle.create_refresh_group()`, `pgtrickle.drop_refresh_group()`, `pgtrickle.refresh_groups()`) for the Cross-Source Snapshot Consistency catalog table introduced in the `0.8.0→0.9.0` upgrade script | ~2–3d | ✅ Done | [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](plans/sql/PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) |

> **Advanced Capabilities subtotal: ~11–13 weeks**

### DVM Engine Correctness & Performance Hardening (P2)

These items address correctness gaps that silently degrade to full-recompute modes or cause excessive I/O on each differential cycle. All are observable in production workloads.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| P2-1 | **Recursive CTE DRed in DIFFERENTIAL mode.** Currently, any DELETE or UPDATE against a recursive CTE's source in DIFFERENTIAL mode falls back to O(n) full recompute + diff. The Delete-and-Rederive (DRed) algorithm exists for IMMEDIATE mode only. Implement DRed for `DeltaSource::ChangeBuffer` so recursive CTE stream tables in DIFFERENTIAL mode maintain O(delta) cost. | 2–3 wk | ⏭️ Deferred to v0.10.0 | [src/dvm/operators/recursive_cte.rs](src/dvm/operators/recursive_cte.rs) |
| P2-2 | **SUM NULL-transition rescan for FULL OUTER JOIN aggregates.** When `SUM` sits above a FULL OUTER JOIN and rows transition between matched and unmatched states (matched→NULL), the algebraic formula gives 0 instead of NULL, triggering a `child_has_full_join()` full-group rescan on every cycle where rows cross that boundary. Implement a targeted correction that avoids full-group rescans in the common case. | 1–2 wk | ⏭️ Deferred to v0.10.0 | [src/dvm/operators/aggregate.rs](src/dvm/operators/aggregate.rs) |
| P2-3 | **DISTINCT multiplicity-count JOIN overhead.** Every differential refresh for `SELECT DISTINCT` queries joins against the stream table's `__pgt_count` column for the full stream table, even when only a tiny delta is being processed. Replace with a per-affected-row lookup pattern to limit this to O(delta) I/O. | 1 wk | ✅ Done | [src/dvm/operators/distinct.rs](src/dvm/operators/distinct.rs) |
| P2-4 | **Materialized view sources in IMMEDIATE mode (EC-09).** Stream tables that use a PostgreSQL materialized view as a source are rejected at creation time when IMMEDIATE mode is requested. Implement a polling-change-detection wrapper (same approach as EC-05 for foreign tables) to support `REFRESH MATERIALIZED VIEW`-sourced queries in IMMEDIATE mode. | 2–3 wk | ⏭️ Deferred to v0.10.0 | [plans/PLAN_EDGE_CASES.md §EC-09](plans/PLAN_EDGE_CASES.md) |
| P2-5 | **`changed_cols` bitmask captured but not consumed in delta scan SQL.** Every CDC change buffer row stores a `changed_cols BIGINT` bitmask recording which source columns were modified by an UPDATE. The DVM delta scan CTE reads every UPDATE row regardless of whether any query-referenced column actually changed. Implement a demand-propagation pass to identify referenced columns per Scan, then inject a `changed_cols & referenced_mask != 0` filter into the delta CTE WHERE clause. For wide source tables (50+ columns) where a typical UPDATE touches 1–3 columns, this eliminates ~98% of UPDATE rows entering the join/aggregate pipeline. | 2–3 wk | ✅ Done | [src/dvm/operators/scan.rs](src/dvm/operators/scan.rs) · [plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md §Task 3.1](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) |
| P2-6 | **LATERAL subquery inner-source change triggers O(\|outer table\|) full re-execution.** When any inner source has CDC entries in the current window, `build_inner_change_branch()` re-materializes the entire outer table snapshot and re-executes the lateral subquery for every outer row — O(\|outer\|) per affected cycle. Gate the outer-table scan behind a join to the inner delta rows so only outer rows correlated with changed inner rows are re-executed. (The analogous scalar subquery fix is P3-3; this is the lateral equivalent.) | 1–2 wk | ⏭️ Deferred to v0.10.0 | [src/dvm/operators/lateral_subquery.rs](src/dvm/operators/lateral_subquery.rs) |
| P2-7 | **Delta predicate pushdown not implemented.** WHERE predicates from the defining query are not pushed into the change buffer scan CTE. A stream table defined as `SELECT … FROM orders WHERE status = 'shipped'` reads all changes from `pgtrickle_changes.changes_<oid>` then filters — for 10K changes/cycle with 50 matching the predicate, 9,950 rows traverse the join/aggregate pipeline needlessly. Collect pushable predicates from the Filter node above the Scan; inject `new_<col> / old_<col>` predicate variants into the delta scan SQL. Care required: UPDATE rows need both old and new column values checked to avoid missing deletions that move rows out of the predicate window. | 2–3 wk | ✅ Done | [src/dvm/operators/scan.rs](src/dvm/operators/scan.rs) · [src/dvm/operators/filter.rs](src/dvm/operators/filter.rs) · [plans/performance/PLAN_NEW_STUFF.md §B-2](plans/performance/PLAN_NEW_STUFF.md) |

> **DVM hardening (P2) subtotal: ~6–9 weeks**

### DVM Performance Trade-offs (P3)

These items are correct as implemented but scale with data size rather than delta size. They are lower priority than P2 but represent solid measurable wins for high-cardinality workloads.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| P3-1 | **Window partition full recompute.** Any single-row change in a window partition triggers recomputation of the entire partition. Add a partition-size heuristic: if the affected partition exceeds a configurable row threshold, downgrade to FULL refresh for that cycle and emit a `pgrx::info!()` message. At minimum, document the O(partition_size) cost prominently. | 1 wk | ✅ Done (documented) | [src/dvm/operators/window.rs](src/dvm/operators/window.rs) |
| P3-2 | **Welford auxiliary columns for CORR/COVAR/REGR_\* aggregates.** `CORR`, `COVAR_POP`, `COVAR_SAMP`, `REGR_*` currently use O(group_size) group-rescan. Implement Welford-style auxiliary column accumulation (`__pgt_aux_sumx_*`, `__pgt_aux_sumy_*`, `__pgt_aux_sumxy_*`) to reach O(1) algebraic maintenance identical to the STDDEV/VAR path. | 2–3 wk | ⏭️ Deferred to v0.10.0 | [src/dvm/operators/aggregate.rs](src/dvm/operators/aggregate.rs) |
| P3-3 | **Scalar subquery C₀ EXCEPT ALL scan.** Part 2 of the scalar subquery delta computes `C₀ = C_current EXCEPT ALL Δ_inserts UNION ALL Δ_deletes` by scanning the full outer snapshot. For large outer tables with an unstable inner source, this scan is proportional to the outer table size. Profile and gate the scan behind an existence check on inner-source stability to avoid it when possible; the `WHERE EXISTS (SELECT 1 FROM delta_subquery)` guard already handles the trivial case. | 1 wk | ✅ Done | [src/dvm/operators/scalar_subquery.rs](src/dvm/operators/scalar_subquery.rs) |
| P3-4 | **Index-aware MERGE planning.** For small deltas against large stream tables (e.g. 5 delta rows, 10M-row ST), the PostgreSQL planner often chooses a sequential scan of the stream table for the MERGE join on `__pgt_row_id`, yielding O(n) full-table I/O when an index lookup would be O(log n). Emit `SET LOCAL enable_seqscan = off` within the MERGE transaction when the delta row count is below a configurable threshold fraction of the ST row count (`pg_trickle.merge_seqscan_threshold` GUC, default 0.001). | 1–2 wk | ✅ Done | [src/refresh.rs](src/refresh.rs) · [src/config.rs](src/config.rs) · [plans/performance/PLAN_NEW_STUFF.md §A-4](plans/performance/PLAN_NEW_STUFF.md) |
| P3-5 | **`auto_backoff` GUC for falling-behind stream tables.** EC-11 implemented the `scheduler_falling_behind` NOTIFY alert at 80% of the refresh budget. The companion `auto_backoff` GUC that automatically doubles the effective refresh interval when a stream table consistently runs behind was explicitly deferred. Add a `pg_trickle.auto_backoff` bool GUC (default off); when enabled, track a per-ST exponential backoff factor in scheduler shared state and reset it on the first on-time cycle. Saves CPU runaway when operators are offline to respond manually. | 1–2d | ✅ Done | [src/scheduler.rs](src/scheduler.rs) · [src/config.rs](src/config.rs) · [plans/PLAN_EDGE_CASES.md §EC-11](plans/PLAN_EDGE_CASES.md) |

> **DVM performance trade-offs (P3) subtotal: ~4–7 weeks**

### Documentation Gaps (D)

| Item | Description | Effort | Status |
|------|-------------|--------|--------|
| D1 | **Recursive CTE DIFFERENTIAL mode limitation.** The O(n) fallback for mixed DELETE/UPDATE against a recursive CTE source is not documented in [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) or [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md). Users hitting DELETE/UPDATE-heavy workloads on recursive CTE stream tables will see unexpectedly slow refresh times with no explanation. Add a "Known Limitations" callout in both files. | ~2h | ✅ Done |
| D2 | **`pgt_refresh_groups` catalog table undocumented.** The catalog table added in the `0.8.0→0.9.0` upgrade script is not described in [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md). Even before the full A8 API lands, document the table schema, its purpose, and the manual INSERT/DELETE workflow users can use in the interim. | ~2h | ✅ Done |

> **v0.9.0 total: ~23–29 weeks**

**Exit criteria:**
- [x] AVG algebraic path implemented (SUM/COUNT auxiliary columns)
- [x] STDDEV/VAR algebraic path implemented (sum-of-squares decomposition)
- [x] MIN/MAX boundary case (delete-the-extremum) covered by property-based tests
- [x] Non-decomposable fallback confirmed (group-rescan strategy)
- [x] Auxiliary columns hidden from user queries via `__pgt_*` naming convention
- [x] Migration path for existing aggregate stream tables tested
- [x] Floating-point drift reset mechanism in place (periodic recompute)
- [x] E2E integration tests for algebraic aggregate paths
- [x] B2-1: Top-K queries (LIMIT/OFFSET/ORDER BY) support
- [x] B2-2: LATERAL Joins support
- [x] B2-3: View Inlining support
- [x] B2-4: Synchronous / Transactional IVM mode
- [x] B2-5: Cross-Source Snapshot Consistency models
- [x] B2-6: Non-Determinism Guarding semantics implemented
- [x] Extension upgrade path tested (`0.8.0 → 0.9.0`)
- [x] G1 Correctness Gaps addressed (G1.1, G1.2, G1.5, G1.6)
- [x] G5 Syntax Gaps addressed (G5.2, G5.3, G5.5, G5.6)
- [x] G6 Test Coverage expanded (G6.1, G6.2, G6.3, G6.5)
- [x] F15: Selective CDC Column Capture (optimize I/O by only tracking columns referenced in query lineage) 
- [x] F40: Extension Upgrade Migration Scripts (finalize versioned SQL schema migrations)
- [x] B3-1: Delta-branch pruning for zero-change sources (skip UNION ALL branch when source has no changes)
- [x] B3-2: Merged-delta weight aggregation — **implemented in v0.10.0** (weight aggregation replaces DISTINCT ON; B3-3 property tests verify correctness)
- [x] B3-3: Property-based correctness tests for B3-2 — **implemented in v0.10.0** (6 diamond-flow E2E property tests)
- [x] EC-03: WARNING emitted when window-in-expression query silently falls back from DIFFERENTIAL to FULL refresh mode
- [x] A8: `pgt_refresh_groups` SQL API (`pgt_add_refresh_group`, `pgt_remove_refresh_group`, `pgt_list_refresh_groups`)
- [x] P2-1: Recursive CTE DRed for DIFFERENTIAL mode — **deferred to v0.10.0** (high risk; ChangeBuffer mode lacks old-state context for safe rederivation; recomputation fallback is correct)
- [x] P2-2: SUM NULL-transition rescan optimization — **deferred to v0.10.0** (requires auxiliary nonnull-count columns; current rescan approach is correct)
- [x] P2-3: DISTINCT `__pgt_count` lookup scoped to O(delta) I/O per cycle
- [x] P2-4: Materialized view sources in IMMEDIATE mode — **deferred to v0.10.0** (requires external polling-change-detection wrapper; out of scope for v0.9.0)
- [x] P3-1: Window partition O(partition_size) cost documented; heuristic downgrade implemented or explicitly deferred
- [x] P3-2: CORR/COVAR_*/REGR_* Welford auxiliary columns — **explicitly deferred to v0.10.0** (group-rescan strategy already works correctly for all regression/correlation aggregates)
- [x] P3-3: Scalar subquery C₀ EXCEPT ALL scan gated behind inner-source stability check or explicitly deferred
- [x] D1: Recursive CTE DIFFERENTIAL mode limitation documented in SQL_REFERENCE.md and DVM_OPERATORS.md
- [x] D2: `pgt_refresh_groups` table schema and interim workflow documented in SQL_REFERENCE.md
- [x] G-1: `panic!()` replaced with `pgrx::error!()` in `source_gates()` and `watermarks()` SQL functions
- [x] G-2 (P2-5): `changed_cols` bitmask consumed in delta scan CTE — referenced-column mask filter injected
- [x] G-3 (P2-6): LATERAL subquery inner-source scoping — **deferred to v0.10.0** (requires correlation predicate extraction from raw SQL; full re-execution is correct)
- [x] G-4 (P2-7): Delta predicate pushdown implemented (pushable predicates injected into change buffer scan CTE)
- [x] G-5 (P3-4): Index-aware MERGE planning: `SET LOCAL enable_seqscan = off` for small deltas against large STs
- [x] G-6 (P3-5): `auto_backoff` GUC implemented; scheduler doubles interval when stream table falls behind

---

## v0.10.0 — DVM Hardening, Connection Pooler Compatibility, Core Refresh Optimizations & Infrastructure Prep

**Status: Released (2026-03-23).**

**Goal:** Land deferred DVM correctness and performance improvements
(recursive CTE DRed, FULL OUTER JOIN aggregate fix, LATERAL scoping,
Welford regression aggregates, multi-source delta merging), fix a class of
post-audit DVM safety issues (SQL comment injection as FROM fragments, silent
wrong aggregate results, EC-01 gap for complex join trees) and CDC correctness
bug (NULL-unsafe PK join, TRUNCATE+INSERT race, stale WAL publication after
partitioning), deliver the first wave of refresh performance optimizations
(index-aware MERGE, predicate pushdown, change buffer compaction, cost-based
refresh strategy), enable cloud-native PgBouncer transaction-mode deployments
via an opt-in compatibility mode, and complete the pre-1.0 packaging
and deployment infrastructure.

### Connection Pooler Compatibility

> **In plain terms:** PgBouncer is the most widely used PostgreSQL connection
> pooler — it sits in front of the database and reuses connections across
> many application threads. In its common "transaction mode" it hands a
> different physical connection to each transaction, which breaks anything
> that assumes the same connection persists between calls (session locks,
> prepared statements). This work introduces an opt-in compatibility mode for
> pg_trickle so it works correctly in cloud deployments — Supabase, Railway,
> Neon, and similar platforms that route through PgBouncer by default.

pg_trickle uses session-level advisory locks and `PREPARE` statements that are
incompatible with PgBouncer transaction-mode pooling. This section introduces an opt-in graceful degradation layer for connection pooler compatibility.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| PB1 | Replace `pg_advisory_lock()` with catalog row-level locking (`FOR UPDATE SKIP LOCKED`) | 3–4d | ✅ Done (0.10-adjustments) | [PLAN_PG_BOUNCER.md](plans/ecosystem/PLAN_PG_BOUNCER.md) |
| PB2 | Add `pooler_compatibility_mode` catalog column directly to `pgt_stream_tables` via `CREATE STREAM TABLE ... WITH (...)` or `alter_stream_table()` to bypass `PREPARE` statements and skip `NOTIFY` locally | 3–4d | ✅ Done (0.10-adjustments) | [PLAN_PG_BOUNCER.md](plans/ecosystem/PLAN_PG_BOUNCER.md) |
| PB3 | E2E validation against PgBouncer transaction-mode (Docker Compose with pooler sidecar) | 1–2d | ✅ Done (0.10-adjustments) | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-28 |

> ⚠️ PB1 — **`SKIP LOCKED` fails silently, not safely.** `pg_advisory_lock()` blocks until the lock is granted, guaranteeing mutual exclusion. `FOR UPDATE SKIP LOCKED` returns **zero rows immediately** if the row is already locked — meaning a second worker will simply not acquire the lock and proceed as if uncontested, potentially running a concurrent refresh on the same stream table. Before merging PB1, verify that every call site that previously relied on the blocking guarantee now explicitly handles the "lock not acquired" path (e.g. skip this cycle and retry) rather than silently proceeding. The E2E test in PB3 must include a concurrent-refresh scenario that would fail if the skip-and-proceed bug is present.

> **PgBouncer compatibility subtotal: ~7–10 days**

### DVM Correctness & Performance (deferred from v0.9.0)

> **In plain terms:** These items were evaluated during v0.9.0 and deferred
> because the current implementations are **correct** — they just scale with
> data size rather than delta size in certain edge cases. All produce correct
> results today; this work makes them faster.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| P2-1 | **Recursive CTE DRed in DIFFERENTIAL mode.** DELETE/UPDATE against a recursive CTE source falls back to O(n) full recompute + diff. Implement DRed for `DeltaSource::ChangeBuffer` to maintain O(delta) cost. | 2–3 wk | ✅ Done (0.10-adjustments) | [src/dvm/operators/recursive_cte.rs](src/dvm/operators/recursive_cte.rs) |
| P2-2 | **SUM NULL-transition rescan for FULL OUTER JOIN aggregates.** When SUM sits above a FULL OUTER JOIN and rows transition between matched/unmatched states, algebraic formula gives 0 instead of NULL, triggering full-group rescan. Implement targeted correction. | 1–2 wk | ✅ Done | [src/dvm/operators/aggregate.rs](src/dvm/operators/aggregate.rs) |
| P2-4 | **Materialized view sources in IMMEDIATE mode (EC-09).** Implement polling-change-detection wrapper for `REFRESH MATERIALIZED VIEW`-sourced queries in IMMEDIATE mode. | 2–3 wk | ✅ Done | [plans/PLAN_EDGE_CASES.md §EC-09](plans/PLAN_EDGE_CASES.md) |
| P2-6 | **LATERAL subquery inner-source scoped re-execution.** Gate outer-table scan behind a join to inner delta rows so only correlated outer rows are re-executed, reducing O(\|outer\|) to O(delta). | 1–2 wk | ✅ Done | [src/dvm/operators/lateral_subquery.rs](src/dvm/operators/lateral_subquery.rs) |
| P3-2 | **Welford auxiliary columns for CORR/COVAR/REGR_\* aggregates.** Implement Welford-style accumulation to reach O(1) algebraic maintenance identical to the STDDEV/VAR path. | 2–3 wk | ✅ Done | [src/dvm/operators/aggregate.rs](src/dvm/operators/aggregate.rs) |
| B3-2 | **Merged-delta weight aggregation.** `GROUP BY __pgt_row_id, SUM(weight)` for cross-source deduplication; remove zero-weight rows. | 3–4 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |
| B3-3 | **Property-based correctness tests** for simultaneous multi-source changes; diamond-flow scenarios. Hard prerequisite for B3-2. | 1–2 wk | ✅ Done | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |

> ✅ B3-2 correctly uses weight aggregation (`GROUP BY __pgt_row_id, SUM(weight)`) instead
> of `DISTINCT ON`. B3-3 property-based tests verify correctness for 6 diamond-flow
> topologies (inner join, left join, full join, aggregate, multi-root, deep diamond).

> **DVM deferred items subtotal: ~12–19 weeks**

### DVM Safety Fixes & CDC Correctness Hardening

These items were identified during a post-v0.9.0 audit of the DVM engine and CDC pipeline. **P0 items produce runtime PostgreSQL syntax errors with no helpful extension-level error; P1 items produce silent wrong results.** They target uncommon query shapes but are fully reachable by users without warning.

#### SQL Comment Injection (P0)

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| SF-1 | **`build_snapshot_sql` catch-all returns an SQL comment as a FROM clause fragment.** The `_` arm of `build_snapshot_sql()` returns `/* unsupported snapshot for <node> */` which is injected directly into JOIN SQL, producing a PostgreSQL syntax error (`syntax error at or near "/"`) instead of a clear extension error. Affects any `RecursiveCte`, `Except`, `Intersect`, `UnionAll`, `LateralSubquery`, `LateralFunction`, `ScalarSubquery`, `Distinct`, or `RecursiveSelfRef` node appearing as a direct JOIN child. Replace the catch-all arm with `PgTrickleError::UnsupportedQuery`. | 0.5 d | ✅ Done | [src/dvm/operators/join_common.rs](src/dvm/operators/join_common.rs) |
| SF-2 | **Explicit `/* unsupported snapshot for distinct */` string in join.rs.** Hardcoded variant of SF-1 for the `Distinct`-child case in inner-join snapshot construction. Same fix: return `PgTrickleError::UnsupportedQuery`. | 0.5 d | ✅ Done | [src/dvm/operators/join.rs](src/dvm/operators/join.rs) |
| SF-3 | **`parser.rs` FROM-clause deparser fallbacks inject SQL comments.** `/* unsupported RangeSubselect */` and `/* unsupported FROM item */` are emitted as FROM clause fragments, causing PostgreSQL syntax errors when the generated SQL is executed. Replace with `PgTrickleError::UnsupportedQuery`. | 0.5 d | ✅ Done | [src/dvm/parser.rs](src/dvm/parser.rs) |

#### DVM Correctness Bugs (P1)

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| SF-4 | **`child_to_from_sql` returns `None` for renamed-column `Project` nodes, silently skipping group rescan.** When a `Project` with column renames (e.g. `EXTRACT(year FROM orderdate) AS o_year`) sits between an aggregate and its source, `child_to_from_sql()` returns `None` and the group-rescan CTE is omitted without error. Groups crossing COUNT 0→1 or MAX deletion thresholds produce permanently stale aggregate values. Distinct from tracked P2-2 (SUM/FULL OUTER JOIN specific); this affects any complex projection above an aggregate. | 1–2 wk | ✅ Done | [src/dvm/operators/aggregate.rs](src/dvm/operators/aggregate.rs) |
| SF-5 | **EC-01 fix is incomplete for right-side join subtrees with ≥3 scan nodes.** `use_pre_change_snapshot()` applies a `join_scan_count(child) <= 2` threshold to avoid cascading CTE materialization. For right-side join chains with ≥3 scan nodes (TPC-H Q7, Q8, Q9 all qualify), the original EC-01 phantom-row-after-DELETE bug is still present. The roadmap marks EC-01 as "Done" without noting this remaining boundary. Extend the fix to ≥3-scan right subtrees, or document the limitation explicitly with a test that asserts the boundary. | 2–3 wk | ✅ Done (boundary documented with 5 unit tests + DVM_OPERATORS.md limitation note) | [src/dvm/operators/join_common.rs](src/dvm/operators/join_common.rs) |
| SF-6 | **EXCEPT `__pgt_count` columns not forwarded through `Project` nodes, causing silent wrong results.** EXCEPT uses a "retain but mark invisible" design (never emits `'D'` events). A `Project` above `EXCEPT` that does not propagate `__pgt_count_l`/`__pgt_count_r` prevents the MERGE step from distinguishing visible from invisible rows. Enforce count column propagation in the planner or raise `PgTrickleError` at planning time if a `Project` over `Except` drops these columns. | 1–2 wk | ✅ Done | [src/dvm/operators/project.rs](src/dvm/operators/project.rs) |

#### DVM Edge-Condition Correctness (P2)

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| SF-7 | **Empty `subquery_cols` silently emits `(SELECT NULL FROM …)` as scalar subquery result.** When inner column detection fails (e.g. star-expansion from a view source), `scalar_col` is set to `"NULL"` and NULL values silently propagate into the stream table with no error raised. Detect empty `subquery_cols` at planning time and return `PgTrickleError::UnsupportedQuery`. | 0.5 d | ✅ Done | [src/dvm/operators/scalar_subquery.rs](src/dvm/operators/scalar_subquery.rs) |
| SF-8 | **Dummy `row_id = 0` in lateral inner-change branch can hash-collide with a real outer row.** `build_inner_change_branch()` emits `0::BIGINT AS __pgt_row_id` as a placeholder for re-executed outer rows. Since actual row hashes span the full BIGINT range, a real outer row could hash to `0`, causing the DISTINCT/MERGE step to conflate it with the dummy entry. Use a sentinel outside the hash range (e.g. `(-9223372036854775808)::BIGINT`, i.e. `MIN(BIGINT)`) or add a separate `__pgt_is_inner_dummy BOOLEAN` discriminator column. | 1 wk | ✅ Done (sentinel changed to i64::MIN) | [src/dvm/operators/lateral_subquery.rs](src/dvm/operators/lateral_subquery.rs) |

#### CDC Correctness (P1–P2)

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| SF-9 | **UPDATE trigger uses `=` (not `IS NOT DISTINCT FROM`) on composite PK columns, silently dropping rows with NULL PK columns.** The `__pgt_new JOIN __pgt_old ON pk_a = pk_a AND pk_b = pk_b` uses `=`, so `NULL = NULL` evaluates to false and those rows are silently dropped from the change buffer. The stream table permanently diverges from the source with no error. Change all PK join conditions in the UPDATE trigger to use `IS NOT DISTINCT FROM`. | 0.5 d | ✅ Done | [src/cdc.rs](src/cdc.rs) |
| SF-10 | **TRUNCATE marker + same-window INSERT ordering is untested; post-TRUNCATE rows may be missed.** If INSERTs arrive after a TRUNCATE but before the scheduler ticks, the change buffer contains both a `'T'` marker and `'I'` rows. The "TRUNCATE → full refresh → discard buffer" path has no E2E test coverage for this sequencing. A race between the FULL refresh snapshot and in-flight inserts could drop post-TRUNCATE inserted rows. Add a targeted E2E test and verify atomicity of the discard-vs-snapshot sequence. | 0.5 d | ✅ Done (verified: TRUNCATE triggers full refresh which re-reads source; change buffer is discarded atomically within the same transaction) | [src/cdc.rs](src/cdc.rs) |
| SF-11 | **WAL publication goes stale after a source table is later converted to partitioned.** `create_publication()` sets `publish_via_partition_root = true` only at creation time. If a source table is subsequently converted to partitioned, WAL events arrive with child-partition OIDs, causing lookup failures and a silent CDC stall for that table (no error, stream table silently freezes). Detect post-creation partitioning during publication health checks and rebuild the publication entry. | 1–2 wk | ✅ Done | [src/wal_decoder.rs](src/wal_decoder.rs) |

#### Operational & Documentation Gaps (P3)

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| SF-12 | **`DiamondSchedulePolicy::Fastest` CPU multiplication is undocumented.** The default policy refreshes all members of a diamond consistency group whenever any member is due. In an asymmetric diamond (B every 1s, C every 5s, both feeding D), C refreshes 5× more often than scheduled, consuming unexplained CPU. Add a cost-implication warning to `CONFIGURATION.md` and `ARCHITECTURE.md`, and explain `DiamondSchedulePolicy::Slowest` as the low-CPU alternative. | 0.5 d | ✅ Done | [src/dag.rs](src/dag.rs) · [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| SF-13 | **ROADMAP inconsistency: B-2 (Delta Predicate Pushdown) listed as ⬜ Not started in v0.10.0 but G-4/P2-7 marked completed in v0.9.0.** The v0.9.0 exit criteria mark `[x] G-4 (P2-7): Delta predicate pushdown implemented`, yet the v0.10.0 table lists `B-2 \| Delta Predicate Pushdown \| ⬜ Not started`. If B-2 has additional scope beyond G-4 (e.g. OR-branch handling for deletions, covering index creation, benchmark targets), document that scope explicitly. If B-2 is fully covered by G-4, remove or mark it done in the v0.10.0 table to avoid double-counting effort. | 0.5 d | ✅ Done (B-2 marked as completed by G-4/P2-7) | [ROADMAP.md](ROADMAP.md) |

> **DVM safety & CDC hardening subtotal: ~3–4 days (SF-1–3, SF-7, SF-9–10, SF-12–13) + ~6–10 weeks (SF-4–6, SF-8, SF-11)**

### Core Refresh Optimizations (Wave 2)

> Read the risk analyses in
> [PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) before implementing.
> Implement in this order: A-4 (no schema change), B-2, C-4, then B-4.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| A-4 | **Index-Aware MERGE Planning.** Planner hint injection (`enable_seqscan = off` for small-delta / large-target); covering index auto-creation on `__pgt_row_id`. No schema changes required. | 1–2 wk | ✅ Done | [PLAN_NEW_STUFF.md §A-4](plans/performance/PLAN_NEW_STUFF.md) |
| B-2 | **Delta Predicate Pushdown.** Push WHERE predicates from defining query into change-buffer `delta_scan` CTE; `OR old_col` handling for deletions; 5–10× delta-row-volume reduction for selective queries. | 2–3 wk | ✅ Done (v0.9.0 as G-4/P2-7) | [PLAN_NEW_STUFF.md §B-2](plans/performance/PLAN_NEW_STUFF.md) |
| C-4 | **Change Buffer Compaction.** Net-change compaction (INSERT+DELETE=no-op; UPDATE+UPDATE=single row); run when buffer exceeds `pg_trickle.compact_threshold`; use advisory lock to serialise with refresh. | 2–3 wk | ✅ Done | [PLAN_NEW_STUFF.md §C-4](plans/performance/PLAN_NEW_STUFF.md) |
| B-4 | **Cost-Based Refresh Strategy.** Replace fixed `differential_max_change_ratio` with a history-driven cost model fitted on `pgt_refresh_history`; cold-start fallback to fixed threshold. | 2–3 wk | ✅ Done (cost model + adaptive threshold already active) | [PLAN_NEW_STUFF.md §B-4](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ C-4: The compaction DELETE **must use `seq` (the sequence primary key) not `ctid`** as
> the stable row identifier. `ctid` changes under VACUUM and will silently delete the wrong
> rows. See the corrected SQL and risk analysis in PLAN_NEW_STUFF.md §C-4.

> ⚠️ A-4 — **Planner hint must be transaction-scoped (`SET LOCAL`), never session-scoped (`SET`).** The existing P3-4 implementation (already shipped) uses `SET LOCAL enable_seqscan = off`, which PostgreSQL automatically reverts at transaction end. Any extension of A-4 (e.g. the covering index auto-creation path) must continue to use `SET LOCAL`. Using plain `SET` instead would permanently disable seq-scans for the remainder of the session, corrupting planner behaviour for all subsequent queries in that backend.

> **Core refresh optimizations subtotal: ~7–11 weeks**

### Scheduler & DAG Scalability

These items address scheduler CPU efficiency and DAG maintenance overhead at scale. Both were identified as C-1 and C-2 in [plans/performance/PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) but were not included in earlier milestones.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| G-7 | **Tiered refresh scheduling (Hot/Warm/Cold/Frozen).** All stream tables currently refresh at their configured interval regardless of how often they are queried. In deployments with many STs, most Cold/Frozen tables consume full scheduler CPU unnecessarily. Introduce four tiers keyed by a per-ST pgtrickle access counter (not `pg_stat_user_tables`, which is polluted by pg_trickle's own MERGE scans): Hot (≥10 reads/min: refresh at configured interval), Warm (1–10 reads/min: ×2 interval), Cold (<1 read/min: ×10 interval), Frozen (0 reads since last N cycles: suspend until manually promoted). A single GUC `pg_trickle.tiered_scheduling` (default off) gates the feature. | 3–4 wk | ✅ Done | [src/scheduler.rs](src/scheduler.rs) · [plans/performance/PLAN_NEW_STUFF.md §C-1](plans/performance/PLAN_NEW_STUFF.md) |
| G-8 | **Incremental DAG rebuild on DDL changes.** Any `CREATE`/`ALTER`/`DROP STREAM TABLE` currently triggers a full O(V+E) re-query of all `pgt_dependencies` rows to rebuild the entire DAG. For deployments with 100+ stream tables this adds per-DDL latency and has a race condition: if two DDL events arrive before the scheduler ticks, only the latest `pgt_id` stored in shared memory may be processed. Replace with a targeted edge-delta approach: the DDL hooks write affected stream table OIDs into a pending-changes queue; the scheduler applies only those edge insertions/deletions, leaving the rest of the graph intact. | 2–3 wk | ✅ Done | [src/dag.rs](src/dag.rs) · [src/scheduler.rs](src/scheduler.rs) · [plans/performance/PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |
| C2-1 | **Ring-buffer DAG invalidation.** Replace single `pgt_id` scalar in shared memory with a bounded ring buffer of affected IDs; full-rebuild fallback on overflow. Hard prerequisite for correctness of G-8 under rapid DDL changes. | 1 wk | ✅ Done | [PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |
| C2-2 | **Incremental topo-sort.** Incremental topo-sort on affected subgraph only; cache sorted schedule in shared memory. | 1–2 wk | ✅ Done | [PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ A single `pgt_id` scalar in shared memory is vulnerable to overwrite when two DDL
> changes arrive between scheduler ticks — use a ring buffer (C2-1) or fall back to full rebuild.
> See PLAN_NEW_STUFF.md §C-2 risk analysis.

> **Scheduler & DAG scalability subtotal: ~7–10 weeks**

### "No Surprises" — Principle of Least Astonishment

> **In plain terms:** pg_trickle does a lot of work automatically — rewriting
> queries, managing auxiliary columns, transitioning CDC modes, falling back
> between refresh strategies. Most of this is exactly what users want, but
> several behaviors happen silently where a brief notification would prevent
> confusion. This section adds targeted warnings, notices, and documentation
> so that every implicit behavior is surfaced to the user at the moment it
> matters.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| NS-1 | **Warn on ORDER BY without LIMIT.** Emit `WARNING` at `create_stream_table` / `alter_stream_table` time when query contains `ORDER BY` without `LIMIT`: "ORDER BY without LIMIT has no effect on stream tables — storage row order is undefined." | 2–4h | ✅ Done | [src/api.rs](src/api.rs) |
| NS-2 | **Warn on append_only auto-revert.** Upgrade the `info!()` to `warning!()` when `append_only` is automatically reverted due to DELETE/UPDATE. Add a `pgtrickle_alert` NOTIFY with category `append_only_reverted`. | 1–2h | ✅ Done | [src/refresh.rs](src/refresh.rs) |
| NS-3 | **Promote cleanup errors after consecutive failures.** Track consecutive `drain_pending_cleanups()` error count in thread-local state; promote from `debug1` to `WARNING` after 3 consecutive failures for the same source OID. | 2–4h | ✅ Done | [src/refresh.rs](src/refresh.rs) |
| NS-4 | **Document `__pgt_*` auxiliary columns in SQL_REFERENCE.** Add a dedicated subsection listing all implicit columns (`__pgt_row_id`, `__pgt_count`, `__pgt_sum`, `__pgt_sum2`, `__pgt_nonnull`, `__pgt_covar_*`, `__pgt_count_l`, `__pgt_count_r`) with the aggregate functions that trigger each. | 2–4h | ✅ Done | [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) |
| NS-5 | **NOTICE on diamond detection with `diamond_consistency='none'`.** When `create_stream_table` detects a diamond dependency and the user hasn't explicitly set `diamond_consistency`, emit `NOTICE`: "Diamond dependency detected — consider setting diamond_consistency='atomic' for consistent cross-branch reads." | 2–4h | ✅ Done | [src/api.rs](src/api.rs) · [src/dag.rs](src/dag.rs) |
| NS-6 | **NOTICE on differential→full fallback.** Upgrade the existing `info!()` in adaptive fallback to `NOTICE` so it appears at default `client_min_messages` level. | 0.5–1h | ✅ Done | [src/refresh.rs](src/refresh.rs) |
| NS-7 | **NOTICE on isolated CALCULATED schedule.** When `create_stream_table` creates an ST with `schedule='calculated'` that has no downstream dependents, emit `NOTICE`: "No downstream dependents found — schedule will fall back to pg_trickle.default_schedule_seconds (currently Ns)." | 1–2h | ✅ Done | [src/api.rs](src/api.rs) |

> **"No Surprises" subtotal: ~10–20 hours**

> **v0.10.0 total: ~58–84 hours + ~32–50 weeks DVM, refresh & safety work + ~10–20 hours "No Surprises"**

**Exit criteria:**
- [x] `ALTER EXTENSION pg_trickle UPDATE` tested (`0.9.0 → 0.10.0`) — upgrade script verified complete via `scripts/check_upgrade_completeness.sh`; adds `pooler_compatibility_mode`, `refresh_tier`, `pgt_refresh_groups`, and updated API function signatures
- [x] All public documentation current and reviewed — SQL_REFERENCE.md, CONFIGURATION.md, CHANGELOG.md, and ROADMAP.md updated for all v0.10.0 features
- [x] G-7: Tiered scheduling (Hot/Warm/Cold/Frozen) implemented; `pg_trickle.tiered_scheduling` GUC gating the feature
- [x] G-8: Incremental DAG rebuild implemented; DDL-triggered edge-delta replaces full O(V+E) re-query
- [x] C2-1: Ring-buffer DAG invalidation safe under rapid consecutive DDL changes
- [x] C2-2: Incremental topo-sort caches sorted schedule; verified by property-based test
- [x] P2-1: Recursive CTE DRed for DIFFERENTIAL mode (O(delta) instead of O(n) recompute) — **implemented in 0.10-adjustments**
- [x] P2-2: SUM NULL-transition correction for FULL OUTER JOIN aggregates — **implemented; `__pgt_aux_nonnull_*` auxiliary column eliminates full-group rescan**
- [x] P2-4: Materialized view sources supported in IMMEDIATE mode
- [x] P2-6: LATERAL subquery inner-source scoped re-execution (O(delta) instead of O(|outer|))
- [x] P3-2: CORR/COVAR_*/REGR_* Welford auxiliary columns for O(1) algebraic maintenance
- [x] B3-2: Merged-delta weight aggregation passes property-based correctness proofs — **implemented; replaces DISTINCT ON with GROUP BY + SUM(weight) + HAVING**
- [x] B3-3: Property-based tests for simultaneous multi-source changes — **implemented; 6 diamond-flow E2E property tests**
- [x] A-4: Covering index auto-created on `__pgt_row_id` with INCLUDE clause for ≤8-column schemas; planner hint prevents seq-scan on small delta; `SET LOCAL` confirmed (not `SET`) so hint reverts at transaction end
- [x] B-2: Predicate pushdown reduces delta volume for selective queries — `bench_b2_predicate_pushdown` in `e2e_bench_tests.rs` measures median filtered vs unfiltered refresh time; asserts filtered ≤3× unfiltered (in practice typically faster)
- [x] C-4: Compaction uses `change_id` PK (not `ctid`); correct under concurrent VACUUM; serialised with advisory lock; net-zero elimination + intermediate row collapse
- [x] B-4: Cost model self-calibrates from refresh history (`estimate_cost_based_threshold` + `compute_adaptive_threshold` with 60/40 blend); cold-start fallback to fixed GUC threshold
- [x] PB1: Concurrent-refresh scenario covered by `test_pb1_concurrent_refresh_skip_locked_no_corruption` in `e2e_concurrent_tests.rs`; two concurrent `refresh_stream_table()` calls verified to produce correct data without corruption; `SKIP LOCKED` path confirmed non-blocking
- [x] SF-1: `build_snapshot_sql` catch-all arm uses `pgrx::error!()` instead of injecting an SQL comment as a FROM fragment
- [x] SF-2: Explicit `/* unsupported snapshot for distinct */` string replaced with `PgTrickleError::UnsupportedQuery` in join.rs
- [x] SF-3: `parser.rs` FROM-clause deparser fallbacks replaced with `PgTrickleError::UnsupportedQuery`
- [x] SF-4: `child_to_from_sql` wraps Project in subquery with projected expressions; rescan CTE correctly resolves aliased column names
- [x] SF-5: EC-01 ≤2-scan boundary documented with 5 unit tests asserting the boundary + DVM_OPERATORS.md limitation note explaining the CTE materialization trade-off
- [x] SF-6: `diff_project` forwards `__pgt_count_l`/`__pgt_count_r` through projection when present in child result
- [x] SF-7: Empty `subquery_cols` in scalar subquery returns `PgTrickleError::UnsupportedQuery` rather than emitting `NULL`
- [x] SF-8: Lateral inner-change branch uses `i64::MIN` sentinel instead of `0::BIGINT` as dummy `__pgt_row_id`
- [x] SF-9: UPDATE trigger PK join uses `IS NOT DISTINCT FROM` for all PK columns; NULL-PK rows captured correctly
- [x] SF-10: TRUNCATE + same-window INSERT E2E test passes; post-TRUNCATE rows not dropped
- [x] SF-11: `check_publication_health()` detects post-creation partitioning and rebuilds publication with `publish_via_partition_root = true`
- [x] SF-12: `DiamondSchedulePolicy::Fastest` cost-multiplication documented in `CONFIGURATION.md` with `Slowest` explanation
- [x] SF-13: B-2 / G-4 roadmap inconsistency resolved; entry reflects actual remaining scope (or marked done if fully completed)
- [x] NS-1: `ORDER BY` without `LIMIT` emits `WARNING` at creation time; E2E test verifies message
- [x] NS-2: `append_only` auto-revert uses `WARNING` (not `INFO`) and sends `pgtrickle_alert` NOTIFY
- [x] NS-3: `drain_pending_cleanups` promotes to `WARNING` after 3 consecutive failures per source OID
- [x] NS-4: `__pgt_*` auxiliary columns documented in SQL_REFERENCE with triggering aggregate functions
- [x] NS-5: Diamond detection with `diamond_consistency='none'` emits `NOTICE` suggesting `'atomic'`
- [x] NS-6: Differential→full adaptive fallback uses `NOTICE` (not `INFO`)
- [x] NS-7: Isolated `CALCULATED` schedule emits `NOTICE` with effective fallback interval
- [x] NS-8: `diamond_consistency` default changed to `'atomic'`; catalog DDL, API code comments, and all documentation updated to match actual runtime behavior (API already resolved `NULL` to `Atomic`)

---

## v0.11.0 — Partitioned Stream Tables, Prometheus & Grafana Observability, Safety Hardening & Correctness

**Goal:** Enable stream table storage to be declaratively partitioned (scope MERGE
to affected partitions for 100× I/O reduction on large tables), add per-database worker
quotas for multi-tenant environments, and ship ready-made Prometheus/Grafana
monitoring so the product is externally visible and monitored. Additionally closes
several high-priority correctness, safety, and performance gaps: wide-column bitmask
correctness, anomalous change fuse, external query corpus gating, worker panic hardening,
SemiJoin delta-key pre-filtering, and `block_source_ddl` default flip.

### Partitioned Stream Tables — Storage (A-1)

> **In plain terms:** A 10M-row stream table partitioned into 100 ranges means only
> the 2–3 partitions that actually received changes are touched by MERGE — reducing
> the MERGE scan from 10M rows to ~100K. The partition key must be a user-visible
> column and the refresh path must inject a verified range predicate.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A1-1 | DDL: `CREATE STREAM TABLE … PARTITION BY` declaration; catalog column for partition key | 1–2 wk | [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) |
| A1-2 | Delta inspection: extract min/max of partition key from delta CTE per scheduler tick | 1 wk | [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) |
| A1-3 | MERGE rewrite: inject validated partition-key range predicate or issue per-partition MERGEs via Rust loop | 2–3 wk | [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) |
| A1-4 | E2E benchmarks: 10M-row partitioned ST, 0.1% change rate concentrated in 2–3 partitions | 1 wk | [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ MERGE joins on `__pgt_row_id` (a content hash unrelated to the partition key) —
> partition pruning will **not** activate automatically. A predicate injection step
> is mandatory. See PLAN_NEW_STUFF.md §A-1 risk analysis before starting.

> **Retraction consideration (A-1):** The 5–7 week effort estimate is optimistic. The
> core assumption — that partition pruning can be activated via a `WHERE partition_key
> BETWEEN ? AND ?` predicate — requires the partition key to be a tracked catalog column
> (not currently the case) and a verified range derivation from the delta. The alternative
> (per-partition MERGE loop in Rust) is architecturally sound but requires significant
> catalog and refresh-path changes. A **design spike** (2–4 days) producing a written
> implementation plan must be completed before A1-1 is started. The milestone is at P3 /
> Very High risk and should not block the 1.0 release if the design spike reveals
> additional complexity.

> **Partitioned stream tables subtotal: ~5–7 weeks**

### Multi-Database Scheduler Isolation (C-3)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C3-1 | Per-database worker quotas (`pg_trickle.per_database_worker_quota`); priority ordering (IMMEDIATE > Hot > Warm > Cold); burst capacity up to 150% when other DBs are under budget | 2–3 wk | [PLAN_NEW_STUFF.md §C-3](plans/performance/PLAN_NEW_STUFF.md) |

> **Multi-DB isolation subtotal: ~2–3 weeks**

### Prometheus & Grafana Observability

> **In plain terms:** Most teams already run Prometheus and Grafana to monitor
> their databases. This ships ready-to-use configuration files — no custom
> code, no extension changes — that plug into the standard `postgres_exporter`
> and light up a Grafana dashboard showing refresh latency, staleness, error
> rates, CDC lag, and per-stream-table detail. Also includes Prometheus
> alerting rules so you get paged when a stream table goes stale or starts
> error-looping. A Docker Compose file lets you try the full observability
> stack with a single `docker compose up`.

Zero-code monitoring integration. All config files live in a new
`monitoring/` directory in the main repo (or a separate
`pgtrickle-monitoring` repo). Queries use existing views
(`pg_stat_stream_tables`, `check_cdc_health()`, `quick_health`).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~OBS-1~~ | ~~**Prometheus metrics out of the box.**~~ ✅ Done in v0.11.0 Phase 3 — `monitoring/prometheus/pg_trickle_queries.yml` exports 14 metrics (per-table refresh stats, health summary, CDC buffer sizes, status counts, recent error rate) via postgres_exporter. | — | [monitoring/prometheus/pg_trickle_queries.yml](monitoring/prometheus/pg_trickle_queries.yml) |
| ~~OBS-2~~ | ~~**Get paged when things go wrong.**~~ ✅ Done in v0.11.0 Phase 3 — `monitoring/prometheus/alerts.yml` has 8 alerting rules: staleness > 5 min, ≥3 consecutive failures, table SUSPENDED, CDC buffer > 1 GB, scheduler down, high refresh duration, cluster WARNING/CRITICAL. | — | [monitoring/prometheus/alerts.yml](monitoring/prometheus/alerts.yml) |
| ~~OBS-3~~ | ~~**See everything at a glance.**~~ ✅ Done in v0.11.0 Phase 3 — `monitoring/grafana/dashboards/pg_trickle_overview.json` has 6 sections: cluster overview stat panels, refresh performance time-series, staleness heatmap, CDC health graphs, per-table drill-down table with schema/table variable filters. | — | [monitoring/grafana/dashboards/pg_trickle_overview.json](monitoring/grafana/dashboards/pg_trickle_overview.json) |
| ~~OBS-4~~ | ~~**Try it all in one command.**~~ ✅ Done in v0.11.0 Phase 3 — `monitoring/docker-compose.yml` spins up PostgreSQL + pg_trickle + postgres_exporter + Prometheus + Grafana with pre-wired config and demo seed data (`monitoring/init/01_demo.sql`). `docker compose up` → Grafana at :3000. | — | [monitoring/docker-compose.yml](monitoring/docker-compose.yml) |

> **Observability subtotal: ~12 hours** ✅

### Default Tuning & Safety Defaults (from REPORT_OVERALL_STATUS.md)

These four changes flip conservative defaults to the behavior that is safe and
correct in production. All underlying features are implemented and tested;
only the default values change. Each keeps the original GUC so operators can
revert if needed.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DEF-1~~ | ~~**Flip `parallel_refresh_mode` default to `'on'`.**~~ ✅ Done in v0.11.0 Phase 1 — default flipped; `normalize_parallel_refresh_mode` maps `None`/unknown → `On`; unit test renamed to `defaults_to_on`. | — | [REPORT_OVERALL_STATUS.md §R1](plans/performance/REPORT_OVERALL_STATUS.md) |
| DEF-2 | ~~**Flip `auto_backoff` default to `true`.**~~ ✅ Done in v0.10.0 — default flipped to `true`; trigger threshold raised to 95%, cap reduced to 8×, log level raised to WARNING. CONFIGURATION.md updated. | 1–2h | [REPORT_OVERALL_STATUS.md §R10](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~DEF-3~~ | ~~**SemiJoin delta-key pre-filter (O-1).**~~ ✅ Verified already implemented in v0.11.0 Phase 2 — `left_snapshot_filtered` pre-filter with `WHERE left_key IN (SELECT DISTINCT right_key FROM delta)` was already present in `semi_join.rs`. | — | [src/dvm/operators/semi_join.rs](src/dvm/operators/semi_join.rs) |
| ~~DEF-4~~ | ~~**Increase invalidation ring capacity from 32 to 128 slots.**~~ ✅ Done in v0.11.0 Phase 1 — `INVALIDATION_RING_CAPACITY` raised to 128 in `shmem.rs`. | — | [REPORT_OVERALL_STATUS.md §R9](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~DEF-5~~ | ~~**Flip `block_source_ddl` default to `true`.**~~ ✅ Done in v0.11.0 Phase 1 — default flipped to `true`; both error messages in `hooks.rs` include step-by-step escape-hatch procedure. | — | [REPORT_OVERALL_STATUS.md §R12](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Default tuning subtotal: ~14–21 hours**

### Safety & Resilience Hardening (Must-Ship)

> **In plain terms:** The background worker should never silently hang or leave a
> stream table in an undefined state when an internal operation fails. These items
> replace `panic!`/`unwrap()` in code paths reachable from the background worker
> with structured errors and graceful recovery.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~SAF-1~~ | ~~**Replace worker-path panics with structured errors.**~~ ✅ Done in v0.11.0 Phase 1 — full audit of `scheduler.rs`, `refresh.rs`, `hooks.rs`: no `panic!`/`unwrap()` outside `#[cfg(test)]`. `check_skip_needed` now logs `WARNING` on SPI error with table name and error details. Audit finding documented in comment. | — | [src/scheduler.rs](src/scheduler.rs) |
| ~~SAF-2~~ | ~~**Failure-injection E2E test.**~~ ✅ Done in v0.11.0 Phase 2 — two E2E tests in `tests/e2e_safety_tests.rs`: (1) column drop triggers UpstreamSchemaChanged, verifies scheduler stays alive and other STs continue; (2) source table drop, same verification. | — | [tests/e2e_safety_tests.rs](tests/e2e_safety_tests.rs) |

> **Safety hardening subtotal: ~7–12 hours**

### Correctness & Code Quality Quick Wins (from REPORT_OVERALL_STATUS.md §12–§15)

> **In plain terms:** Six self-contained improvements identified in the deep gap
> analysis. Each takes under a day and substantially reduces silent failure
> modes, operator confusion, and diagnostic friction.

#### Quick Fixes (< 1 hour each)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| QF-1 | ~~**Fix unguarded debug `println!`.**~~ ✅ Done in v0.11.0 Phase 1 — `println!` replaced with `pgrx::log!()` guarded by new `pg_trickle.log_merge_sql` GUC (default `off`). | — | [src/refresh.rs](src/refresh.rs) |
| QF-2 | ~~**Upgrade AUTO mode downgrade log level.**~~ ✅ Done in v0.11.0 Phase 1 — four AUTO→FULL downgrade paths in `api.rs` raised from `pgrx::info!()` to `pgrx::warning!()`. | — | [plans/performance/REPORT_OVERALL_STATUS.md §12](plans/performance/REPORT_OVERALL_STATUS.md) |
| QF-3 | ~~**Warn when `append_only` auto-reverts.**~~ ✅ Verified already implemented — `pgrx::warning!()` + `emit_alert(AppendOnlyReverted)` already present in `refresh.rs`. | — | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| QF-4 | ~~**Document parser `unwrap()` invariants.**~~ ✅ Done in v0.11.0 Phase 1 — `// INVARIANT:` comments added at four `unwrap()` sites in `dvm/parser.rs` (after `is_empty()` guard, `len()==1` guards, and non-empty `Err` return). | — | [src/dvm/parser.rs](src/dvm/parser.rs) |

> **Quick-fix subtotal: ~3–4 hours**

#### Effective Refresh Mode Tracking (G12-ERM)

> **In plain terms:** When a stream table is configured as `AUTO`, operators
> currently have no way to discover which mode is *actually* being used at
> runtime without reading warning logs. Storing the resolved mode in the catalog
> and exposing a diagnostic function closes this observability gap.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G12-ERM-1~~ | ~~Add `effective_refresh_mode` column to `pgt_stream_tables`~~. ✅ Done in v0.11.0 Phase 2 — column added; scheduler writes actual mode (FULL/DIFFERENTIAL/APPEND_ONLY/TOP_K/NO_DATA) via thread-local tracking; upgrade SQL `pg_trickle--0.10.0--0.11.0.sql` created. | — | [src/catalog.rs](src/catalog.rs) |
| ~~G12-ERM-2~~ | ~~Add `explain_refresh_mode(name TEXT)` SQL function~~. ✅ Done in v0.11.0 Phase 2 — `pgtrickle.explain_refresh_mode()` returns configured mode, effective mode, and downgrade reason. | — | [src/api.rs](src/api.rs) |

> **Effective refresh mode subtotal: ~4–7 hours**

#### Correctness Guards (G12-2, G12-AGG)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G12-2~~ | ~~**TopK runtime validation.**~~ ✅ Done in v0.11.0 Phase 4 — `validate_topk_metadata()` re-parses the reconstructed full query on each TopK refresh; `validate_topk_metadata_fields()` validates stored fields (pure logic, unit-testable). Falls back to FULL + `WARNING` on mismatch. 7 unit tests. | — | [src/refresh.rs](src/refresh.rs) |
| ~~G12-AGG~~ | ~~**Group-rescan aggregate warning.**~~ ✅ Done in v0.11.0 Phase 4 — `classify_agg_strategy()` classifies each aggregate as ALGEBRAIC_INVERTIBLE / ALGEBRAIC_VIA_AUX / SEMI_ALGEBRAIC / GROUP_RESCAN. Warning emitted at `create_stream_table` time for DIFFERENTIAL + group-rescan aggs. Strategy exposed in `explain_st()` as `aggregate_strategies` JSON. 18 unit tests. | — | [src/dvm/parser.rs](src/dvm/parser.rs) |

> **Correctness guards subtotal: ✅ Complete**

#### Parameter & Error Hardening (G15-PV, G13-EH)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G15-PV~~ | ~~**Validate incompatible parameter combinations.**~~ ✅ Done in v0.11.0 Phase 2 — (a) `cdc_mode='wal'` + `refresh_mode='IMMEDIATE'` rejection was already present; (b) `diamond_schedule_policy='slowest'` + `diamond_consistency='none'` now rejected in `create_stream_table_impl` and `alter_stream_table_impl` with structured error. | — | [src/api.rs](src/api.rs) |
| ~~G13-EH~~ | ~~**Structured error HINT/DETAIL fields.**~~ ✅ Done in v0.11.0 Phase 2 — `raise_error_with_context()` helper in `api.rs` uses `ErrorReport::new().set_detail().set_hint()` for `UnsupportedOperator`, `CycleDetected`, `UpstreamSchemaChanged`, and `QueryParseError`; all 8 API-boundary error sites updated. | — | [src/api.rs](src/api.rs) |

> **Parameter & error hardening subtotal: ~6–12 hours**

#### Testing: EC-01 Boundary Regression (G17-EC01B-NEG)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G17-EC01B-NEG~~ | ~~Add a negative regression test asserting that ≥3-scan join right subtrees currently fall back to FULL refresh.~~ ✅ Done in v0.11.0 Phase 4 — 4 unit tests in `join_common.rs` covering 3-way join, 4-way join, right-subtree ≥3 scans, and 2-scan boundary. `// TODO: Remove when EC01B-1/EC01B-2 fixed in v0.12.0` | — | [src/dvm/operators/join_common.rs](src/dvm/operators/join_common.rs) |

> **EC-01 boundary regression subtotal: ✅ Complete**

#### Documentation Quick Wins (G16-GS, G16-SM, G16-MQR, G15-GUC)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G16-GS | **Restructure `GETTING_STARTED.md` with progressive complexity.** Five chapters: (1) Hello World — single-table ST with no join; (2) Multi-table join; (3) Scheduling & backpressure; (4) Monitoring — 5 key functions; (5) Advanced — FUSE, wide bitmask, partitions. Remove the current flat wall-of-SQL structure. | ~1–2d | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| G16-SM | **SQL/mode operator support matrix.** Add a table to `docs/DVM_OPERATORS.md`: rows = SQL construct, columns = `FULL` / `DIFFERENTIAL` / `AUTO` / `IMMEDIATE`; cells = ✅ / ⚠️ caveat / ❌ unsupported, with footnotes for caveats. | ~4–8h | [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) |
| G16-MQR | **Monitoring quick reference.** Add a "Monitoring Quick Reference" section to `docs/GETTING_STARTED.md` covering `stream_table_status()`, `change_buffer_stats()`, `refresh_history()`, `refresh_metrics()`, and `scheduler_status()` with one-line examples. | ~2h | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| G15-GUC | **GUC interaction matrix.** Add to `docs/CONFIGURATION.md`: a matrix of GUCs with cross-dependencies (which GUCs interact, which override others), plus three named tuning profiles (low-latency, high-throughput, balanced). | ~4h | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |

> **Documentation subtotal: ~2–3 days**

> **Correctness quick-wins & documentation subtotal: ~1–2 days code + ~2–3 days docs**

### Should-Ship Additions

#### Wider Changed-Column Bitmask (>63 columns)

> **In plain terms:** Stream tables built on source tables with more than 63 columns
> fall back silently to tracking every column on every UPDATE, losing all CDC selectivity.
> Extending the `changed_cols` field from a `BIGINT` to a `BYTEA` vector removes this
> cliff without breaking existing deployments.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| WB-1 | Extend the CDC trigger `changed_cols` column from `BIGINT` to `BYTEA`; update bitmask encoding/decoding in `cdc.rs`; add schema migration for existing change buffer tables (tables with <64 columns are unaffected at the data level). | 1–2 wk | [REPORT_OVERALL_STATUS.md §R13](plans/performance/REPORT_OVERALL_STATUS.md) |
| WB-2 | E2E test: wide (>63 column) source table; verify only referenced columns trigger delta propagation; benchmark UPDATE selectivity before/after. | 2–4h | `tests/e2e_cdc_tests.rs` |

> **Wider bitmask subtotal: ~1–2 weeks + ~4h testing**

#### Fuse — Anomalous Change Detection

> **In plain terms:** A circuit breaker that stops a stream table from processing
> an unexpectedly large batch of changes (runaway script, mass delete, data migration)
> without operator review. A blown fuse halts refresh and emits a `pgtrickle_alert`
> NOTIFY; `reset_fuse()` resumes with a chosen recovery action (`apply`,
> `reinitialize`, or `skip_changes`).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~FUSE-1~~ ✅ | ~~Catalog: fuse state columns on `pgt_stream_tables` (`fuse_mode`, `fuse_state`, `fuse_ceiling`, `fuse_sensitivity`, `blown_at`, `blow_reason`)~~ | 1–2h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| ~~FUSE-2~~ ✅ | ~~`alter_stream_table()` new params: `fuse`, `fuse_ceiling`, `fuse_sensitivity`~~ | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| ~~FUSE-3~~ ✅ | ~~`reset_fuse(name, action => 'apply'\|'reinitialize'\|'skip_changes')` SQL function~~ | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| ~~FUSE-4~~ ✅ | ~~`fuse_status()` introspection function~~ | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| ~~FUSE-5~~ ✅ | ~~Scheduler pre-check: count change buffer rows; evaluate threshold; blow fuse + NOTIFY if exceeded~~ | 2–3h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| ~~FUSE-6~~ ✅ | ~~E2E tests: normal baseline, spike → blow, reset (`apply`/`reinitialize`/`skip_changes`), diamond/DAG interaction~~ | 4–6h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |

> **Fuse subtotal: ~10–14 hours — ✅ Complete**

#### External Correctness Gate (TS1 or TS2)

> **In plain terms:** Run an independent public query corpus through pg_trickle's
> DIFFERENTIAL mode and assert the results match a vanilla PostgreSQL execution.
> This catches blind spots that the extension's own test suite cannot, and
> provides an objective correctness baseline before v1.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TS1 | **sqllogictest suite.** Run the PostgreSQL sqllogic suite through pg_trickle DIFFERENTIAL mode; gate CI on zero correctness mismatches. *Preferred choice: broadest query coverage.* | 2–3d | [PLAN_TESTING_GAPS.md §J](plans/testing/PLAN_TESTING_GAPS.md) |
| TS2 | **JOB (Join Order Benchmark).** Correctness baseline and refresh latency profiling on realistic multi-join analytical queries. *Alternative if sqllogictest setup is too costly.* | 1–2d | [PLAN_TESTING_GAPS.md §J](plans/testing/PLAN_TESTING_GAPS.md) |

Deliver **one** of TS1 or TS2; whichever is completed first meets the exit criterion.

> **External correctness gate subtotal: ~1–3 days**

#### Differential ST-to-ST Refresh

> **In plain terms:** When stream table B's defining query reads from stream
> table A, pg_trickle currently forces a FULL refresh of B every time A
> updates — re-executing B's entire query even when only a handful of rows
> changed. This feature gives ST-to-ST dependencies the same CDC change
> buffer that base tables already have, so B refreshes differentially (applying
> only the delta). Crucially, even when A itself does a FULL refresh, a
> pre/post snapshot diff is captured so B still receives a small I/D delta
> rather than cascading FULL through the chain.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ST-ST-1 | **Change buffer infrastructure.** Add `create_st_change_buffer_table()` / `drop_st_change_buffer_table()` in `cdc.rs`; lifecycle hooks in `api.rs` (create when a STREAM TABLE source is registered, drop when last consumer is removed); auto-create during extension upgrade for all existing ST-to-ST dependencies (idempotent). | 1 wk | [PLAN_ST_TO_ST.md §Phase 1](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-2 | **Delta capture — DIFFERENTIAL path.** In `execute_differential_refresh()`, use `MERGE … RETURNING` (PG 17+) to atomically apply the MERGE and insert delta rows into `changes_pgt_{id}` in a single round-trip. The explicit-DML path reads from the already-materialised `__pgt_delta_{id}` temp table. | 1–2 wk | [PLAN_ST_TO_ST.md §Phase 2](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-3 | **Delta capture — FULL path.** In `execute_full_refresh()`, snapshot the pre-refresh row set into a temp table, run the FULL refresh, then diff pre/post and write I/D pairs to `changes_pgt_{id}`. Downstream STs always receive a differential delta regardless of whether the upstream did FULL or DIFFERENTIAL. Cost is O(N) — accepted tradeoff that eliminates cascading FULL through deep ST chains. | 1 wk | [PLAN_ST_TO_ST.md §7](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-4 | **DVM scan operator for ST sources.** Extend `dvm/operators/scan.rs`: ST sources read from `changes_pgt_{id}` instead of `changes_{oid}`; introduce `pgt_`-prefixed LSN placeholder tokens; extend the frontier computation and placeholder resolver in `refresh.rs`. | 1–2 wk | [PLAN_ST_TO_ST.md §Phase 3](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-5 | **Scheduler integration.** Rewrite `has_stream_table_source_changes()` to check the ST buffer for rows beyond the frontier (mirrors `has_table_source_changes()`). Remove the `has_stream_table_changes → Full` override when upstream has a buffer; retain FULL + timestamp fallback when no buffer exists. | 0.5 wk | [PLAN_ST_TO_ST.md §Phase 4](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-6 | **Cleanup, lifecycle & tests.** Extend `drain_pending_cleanups()` for ST buffers; wire DDL lifecycle hooks in `hooks.rs` for schema changes. Unit tests for buffer lifecycle and DVM scan SQL; integration tests for the ST → ST DIFFERENTIAL chain; E2E tests for 3-level chain, DROP-middle-ST fallback, schema change buffer recreation, and min-frontier cleanup. | 1 wk | [PLAN_ST_TO_ST.md §Phase 5–6](plans/sql/PLAN_ST_TO_ST.md) |

> **ST-to-ST differential subtotal: ~4.5–6.5 weeks**

### Adaptive/Event-Driven Scheduler Wake (Must-Ship)

> **In plain terms:** The scheduler currently wakes on a fixed 1-second timer
> even when nothing has changed. This adds event-driven wake: CDC triggers
> notify the scheduler immediately when changes arrive. Median end-to-end
> latency drops from ~515 ms to ~15 ms for low-volume workloads — a 34×
> improvement. This is a must-ship item because **low latency is a primary
> project goal**.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~WAKE-1~~ | ~~**Event-driven scheduler wake.**~~ ✅ Done in v0.11.0 Phase 7 — CDC triggers emit `pg_notify('pgtrickle_wake', '')` after each change buffer INSERT; scheduler issues `LISTEN pgtrickle_wake` at startup; 10 ms debounce coalesces rapid notifications; poll fallback preserved. New GUCs: `event_driven_wake` (default `true`), `wake_debounce_ms` (default `10`). E2E tests in `tests/e2e_wake_tests.rs`. | — | [REPORT_OVERALL_STATUS.md §R16](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Event-driven wake subtotal: ✅ Complete**

### Stretch Goals (if capacity allows after Must-Ship)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| STRETCH-1 | **Partitioned stream tables — design spike only.** Produce a written implementation plan (2–4 days) before committing to A1-1. Validates partition-key predicate injection, per-partition MERGE loop feasibility, and catalog requirements. Full A-1 implementation only starts if the spike is complete and the RFC is approved. | 2–4d | [PLAN_NEW_STUFF.md §A-1](plans/performance/PLAN_NEW_STUFF.md) |

> **Stretch subtotal: 2–4 days (STRETCH-1 spike)**

> **v0.11.0 total: ~7–10 weeks (partitioning + isolation) + ~12h observability + ~14–21h default tuning + ~7–12h safety hardening + ~2–4 weeks should-ship (bitmask + fuse + external corpus) + ~4.5–6.5 weeks ST-to-ST differential + ~2–3 weeks event-driven wake + ~1–2 days correctness quick-wins + ~2–3 days documentation**

**Exit criteria:**
- [ ] Declaratively partitioned stream tables accepted; partition key tracked in catalog
- [ ] Partition-scoped MERGE benchmark: 10M-row ST, 0.1% change rate (expect ~100× I/O reduction)
- [ ] Per-database worker quotas enforced; burst reclaimed within 1 scheduler cycle
- [ ] Prometheus queries + alerting rules + Grafana dashboard shipped — ✅ Done in v0.11.0 Phase 3 (`monitoring/` directory)
- [x] DEF-1: `parallel_refresh_mode` default is `'on'`; unit test updated — ✅ Done in v0.11.0 Phase 1 (concurrent-refresh E2E test still pending)
- [x] DEF-2: `auto_backoff` default is `true`; CONFIGURATION.md updated — ✅ Done in v0.10.0
- [x] DEF-3: SemiJoin delta-key pre-filter verified already implemented — ✅ Done in v0.11.0 Phase 2 (pre-existing in `semi_join.rs`)
- [x] DEF-4: Invalidation ring capacity is 128 slots — ✅ Done in v0.11.0 Phase 1 (rapid DDL E2E test still pending)
- [x] DEF-5: `block_source_ddl` default is `true`; error message includes escape-hatch instructions — ✅ Done in v0.11.0 Phase 1 (E2E test still pending)
- [x] SAF-1: No `panic!`/`unwrap()` in background worker hot paths; `check_skip_needed` logs SPI errors — ✅ Done in v0.11.0 Phase 1
- [x] SAF-2: Failure-injection E2E tests in `tests/e2e_safety_tests.rs` — ✅ Done in v0.11.0 Phase 2
- [ ] WB-1+2: Changed-column bitmask supports >63 columns; wide-table CDC selectivity E2E passes; schema migration tested
- [ ] FUSE-1–6: Fuse blows on configurable change-count threshold; `reset_fuse()` recovers in all three action modes; diamond/DAG interaction tested
- [ ] TS1 or TS2: At least one external query corpus passes with zero correctness mismatches in DIFFERENTIAL mode
- [x] QF-1–4: `println!` replaced with guarded `pgrx::log!()`; AUTO downgrades emit `WARNING`; `append_only` reversion verified already warns; parser invariant sites annotated — ✅ Done in v0.11.0 Phase 1
- [x] G12-ERM: `effective_refresh_mode` column present in `pgt_stream_tables`; `explain_refresh_mode()` returns configured mode, effective mode, downgrade reason — ✅ Done in v0.11.0 Phase 2
- [x] G12-2: TopK path validates assumptions at refresh time; triggers FULL fallback with `WARNING` on violation — ✅ Done in v0.11.0 Phase 4
- [x] G12-AGG: Group-rescan aggregate warning fires at `create_stream_table` for DIFFERENTIAL mode; strategy visible in `explain_st()` — ✅ Done in v0.11.0 Phase 4
- [x] G15-PV: Incompatible `cdc_mode`/`refresh_mode` and `diamond_schedule_policy` combinations rejected at creation time with structured `HINT` — ✅ Done in v0.11.0 Phase 2
- [x] G13-EH: `UnsupportedOperator`, `CycleDetected`, `UpstreamSchemaChanged`, `QueryParseError` include `DETAIL` and `HINT` fields — ✅ Done in v0.11.0 Phase 2
- [x] G17-EC01B-NEG: Negative regression test documents ≥3-scan fall-back behavior; linked to v0.12.0 EC01B fix — ✅ Done in v0.11.0 Phase 4
- [ ] G16-GS/SM/MQR/GUC: GETTING_STARTED restructured with progressive complexity; DVM_OPERATORS support matrix added; monitoring quick reference added; CONFIGURATION.md GUC matrix added
- [ ] ST-ST-1–6: All ST-to-ST dependencies refresh differentially when upstream has a change buffer; FULL refreshes on an upstream ST produce a pre/post I/D diff so downstream STs never cascade FULL through the chain; auto-migration creates buffers for existing ST-to-ST dependencies on upgrade; 3-level E2E chain test passes
- [x] WAKE-1: Event-driven scheduler wake implemented; latency E2E test shows sub-50ms median response for single-source workloads — ✅ Done in v0.11.0 Phase 7
- [ ] Extension upgrade path tested (`0.10.0 → 0.11.0`)

---

## v0.12.0 — Scalability Foundations, Anomalous Change Detection & CDC Research

**Goal:** Deliver scalability foundations that directly serve the project's
performance and throughput goals — columnar change tracking for 50–90%
delta-volume reduction and shared change buffers for multi-ST deployments.
Also land the anomalous change fuse, conduct the D-2 CDC research spike,
and ship developer tooling and correctness improvements.

### Anomalous Change Detection (Fuse)

> **In plain terms:** Imagine a source table suddenly receives a
> million-row batch delete — a bug, runaway script, or intentional purge.
> Without a fuse, pg_trickle would try to process all of it and potentially
> overload the database. This adds a circuit breaker: you set a ceiling
> (e.g. "never process more than 50,000 changes at once"), and if that
> limit is hit the stream table pauses and sends a notification. You
> investigate, fix the root cause, then resume with `reset_fuse()` and
> choose how to recover (apply the changes, reinitialize from scratch, or
> skip them entirely).

Per-stream-table fuse that blows when the change buffer row count exceeds a
configurable fixed ceiling or an adaptive μ+kσ threshold derived from
`pgt_refresh_history`. A blown fuse halts refresh and emits a
`pgtrickle_alert` NOTIFY; `reset_fuse()` resumes with a chosen recovery
action.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| FUSE-1 | Catalog: fuse state columns on `pgt_stream_tables` (`fuse_mode`, `fuse_state`, `fuse_ceiling`, `fuse_sensitivity`, `blown_at`, `blow_reason`) | 1–2h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| FUSE-2 | `alter_stream_table()` new params: `fuse`, `fuse_ceiling`, `fuse_sensitivity` | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| FUSE-3 | `reset_fuse(name, action => 'apply'\|'reinitialize'\|'skip_changes')` SQL function | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| FUSE-4 | `fuse_status()` introspection function | 1h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| FUSE-5 | Scheduler pre-check: count change buffer rows; evaluate threshold; blow fuse + NOTIFY if exceeded | 2–3h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |
| FUSE-6 | E2E tests: normal baseline, spike → blow, reset, diamond/DAG interaction | 4–6h | [PLAN_FUSE.md](plans/sql/PLAN_FUSE.md) |

> **Anomalous change detection subtotal: ~10–14 hours**

### Correctness — EC-01 Deep Fix (≥3-Scan Join Right Subtrees)

> **In plain terms:** The phantom-row-after-DELETE bug (EC-01) was fixed for join
> children with ≤2 scan nodes on the right side. Wider join chains — TPC-H Q7, Q8,
> Q9 all qualify — are still silently affected: when both sides of a join are deleted
> in the same batch, the DELETE can be silently dropped. The existing EXCEPT ALL
> snapshot strategy causes PostgreSQL to spill multi-GB temp files for deep join
> trees, which is why the threshold exists. This work designs a fundamentally
> different per-subtree snapshot strategy that removes the cap.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC01B-1 | Design and implement a per-subtree CTE-based snapshot strategy to replace EXCEPT ALL for right-side join chains with ≥3 scan nodes; remove the `join_scan_count(child) <= 2` threshold in `use_pre_change_snapshot` | 3–4 wk | [src/dvm/operators/join_common.rs](src/dvm/operators/join_common.rs) · [plans/PLAN_EDGE_CASES.md §EC-01](plans/PLAN_EDGE_CASES.md) |
| EC01B-2 | TPC-H Q7/Q8/Q9 regression tests: combined left-DELETE + right-DELETE in same cycle; assert no phantom-row drop | 1–2d | `tests/e2e_tpch_tests.rs` |

> **EC-01 deep fix subtotal: ~3–4 weeks**

### CDC Write-Side Overhead Benchmark

> **In plain terms:** Every INSERT/UPDATE/DELETE on a source table fires a PL/pgSQL
> trigger that writes to the change buffer. We have never measured how much write
> throughput this costs. These benchmarks quantify it across five scenarios (single-row,
> bulk INSERT, bulk UPDATE, bulk DELETE, concurrent writers) and gate the decision on
> whether to implement a `change_buffer_unlogged` GUC that could reduce WAL overhead
> by ~20–30%.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BENCH-W1 | Implement `tests/e2e_cdc_write_overhead_tests.rs`: compare source-only vs. source + stream table DML throughput across five scenarios; report write amplification factor | 2–3d | [PLAN_TRIGGERS_OVERHEAD.md](plans/performance/PLAN_TRIGGERS_OVERHEAD.md) |
| BENCH-W2 | Publish results in `docs/BENCHMARK.md`; if WAL overhead > 30% of trigger cost, implement `pg_trickle.change_buffer_unlogged` GUC (`'off'` default; `'on'` unlogged; `'auto'` for high-throughput sources) | 1–2d | [PLAN_TRIGGERS_OVERHEAD.md](plans/performance/PLAN_TRIGGERS_OVERHEAD.md) |

> **CDC write-side benchmark subtotal: ~3–5 days**

### Developer Tooling & Observability Functions (from REPORT_OVERALL_STATUS.md §15)

> **In plain terms:** pg_trickle's diagnostic toolbox today is limited to
> `explain_st()` and `refresh_history()`. Operators debugging unexpected mode
> changes, query rewrites, or error patterns must read source code or server
> logs. This section adds four SQL-callable diagnostic functions that surface
> internal state in a structured, queryable form.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DT-1 | **`explain_query_rewrite(query TEXT)`** — parse a query through the DVM pipeline and return the rewritten SQL plus a list of passes applied (operator rewrites, delta-key injections, TopK detection, group-rescan classification). Useful for debugging unexpected refresh behavior without creating a stream table. | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| DT-2 | **`diagnose_errors(name TEXT)`** — return the last 5 error events for a stream table, classified by type (correctness, performance, config, infrastructure), with a suggested remediation for each class. | ~2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| DT-3 | **`list_auxiliary_columns(name TEXT)`** — list all `__pgt_*` internal columns injected into the stream table's query plan with their purpose (delta tracking, row identity, compaction key). Helps users understand unexpected columns in `SELECT *` output. | ~1d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| DT-4 | **`validate_query(query TEXT)`** — parse and run DVM validation on a query without creating a stream table; return the resolved refresh mode, detected SQL constructs (group-rescan aggregates, non-equijoins, multi-scan subtrees), and any warnings. | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Developer tooling subtotal: ~5–8 days**

### Parser Safety, Concurrency & Query Coverage (from REPORT_OVERALL_STATUS.md §13/§12/§17)

> Additional correctness and robustness items from the deep gap analysis:
> a stack-overflow prevention guard for pathological queries, a concurrency
> stress test for IMMEDIATE mode, and two investigations into known under-
> documented query constructs.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G13-SD | **Parser recursion depth limit.** Add a recursion depth counter to all recursive parse-tree visitor functions in `dvm/parser.rs`. Return `PgTrickleError::QueryTooComplex` if depth exceeds `pg_trickle.max_parse_depth` (GUC, default 64). Prevents stack-overflow crashes on pathological queries. | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §13](plans/performance/REPORT_OVERALL_STATUS.md) |
| G17-IMS | **IMMEDIATE mode concurrency stress test.** 100+ concurrent DML transactions on the same source table in `IMMEDIATE` refresh mode; assert zero lost updates, zero phantom rows, and no deadlocks. | ~2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §17](plans/performance/REPORT_OVERALL_STATUS.md) |
| G12-SQL-IN | **Multi-column `IN (subquery)` correctness investigation.** Determine behavior when DVM encounters `EXPR IN (subquery returning multiple columns)`. Add a correctness test; if the construct is broken, fix it or document as unsupported with a structured error. | ~2–4d | [plans/performance/REPORT_OVERALL_STATUS.md §12](plans/performance/REPORT_OVERALL_STATUS.md) |
| G14-MDED | **MERGE deduplication profiling.** Profile how often concurrent-write scenarios produce duplicate key entries requiring pre-MERGE compaction. If ≥10% of refresh cycles need dedup, write an RFC for a two-pass MERGE strategy. | ~3–5d | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |
| G17-MERGEEX | **MERGE template EXPLAIN validation in E2E tests.** Add `EXPLAIN (COSTS OFF)` dry-run checks for generated MERGE SQL templates at E2E test startup. Catches malformed templates before any data is processed. | ~1d | [plans/performance/REPORT_OVERALL_STATUS.md §17](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Parser safety & coverage subtotal: ~9–15 days**

### Differential Fuzzing (SQLancer)

> **In plain terms:** SQLancer is a SQL fuzzer that generates thousands of syntactically
> valid but structurally unusual queries and uses mathematical oracles (NoREC, TLP) to
> prove our DVM engine produces exactly the same results as PostgreSQL's native executor.
> Unlike hand-written tests, it explores the long tail of NULL semantics, nested
> aggregations, and edge cases no human would write. Any backend crash or result
> mismatch becomes a permanent regression test seed.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| SQLANCER-1 | Docker-based harness: `just sqlancer` spins up E2E container; crash-test oracle verifies that no SQLancer-generated `create_stream_table` call crashes the backend | 3–4d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §Steps 1–2 |
| SQLANCER-2 | Equivalence oracle: for each generated query Q, assert `create_stream_table` + `refresh` output equals native `SELECT` (multiset comparison); failures auto-committed as proptest regression seeds | 3–4d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §Step 3 |
| SQLANCER-3 | CI `weekly-sqlancer` job (daily schedule + manual dispatch); new proptest seed files committed on any detected correctness failure | 1–2d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) |

> **SQLancer fuzzing subtotal: ~1–2 weeks**

### Property-Based Invariant Tests (Items 5 & 6)

> **In plain terms:** Items 1–4 of the property test plan are done. These two
> remaining items add topology/scheduler stress tests (random DAG shapes with
> multi-source branch interactions) and pure Rust unit-level properties (ordering
> monotonicity, SCC bookkeeping correctness). Both slot into the existing proptest
> harness and provide coverage that example-based tests cannot exhaustively explore.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PROP-5 | Topology / scheduler stress: randomized DAG topologies with multi-source branch interactions; assert no incorrect refresh ordering or spurious suspension | 4–6d | [PLAN_TEST_PROPERTY_BASED_INVARIANTS.md](plans/testing/PLAN_TEST_PROPERTY_BASED_INVARIANTS.md) §Item 5 |
| PROP-6 | Pure Rust DAG / scheduler helper properties: ordering invariants, monotonic metadata helpers, SCC bookkeeping edge-cases | 2–4d | [PLAN_TEST_PROPERTY_BASED_INVARIANTS.md](plans/testing/PLAN_TEST_PROPERTY_BASED_INVARIANTS.md) §Item 6 |

> **Property testing subtotal: ~6–10 days**

### Async CDC — Research Spike (D-2)

> **In plain terms:** A custom PostgreSQL logical decoding plugin could write changes
> directly to change buffers without the polling round-trip, cutting CDC latency by
> ~10× and WAL decoding CPU by 50–80%. This milestone scopes a research spike only —
> not a full implementation — to validate the key technical constraints.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| D2-R | Research spike: prototype in-memory row buffering inside `pg_trickle_decoder`; validate SPI flush in `commit` callback; document memory-safety constraints and feasibility; produce a written RFC before any full implementation is started | 2–3 wk | [PLAN_NEW_STUFF.md §D-2](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ SPI writes inside logical decoding `change` callbacks are **not supported**.
> All row buffering must occur in-memory within the plugin's memory context; flush
> only in the `commit` callback. In-memory buffers must handle arbitrarily large
> transactions. See PLAN_NEW_STUFF.md §D-2 risk analysis before writing any C code.

> **Retraction candidate (D-2):** Even as a research spike, this item introduces C-level
> complexity (custom output plugin memory management, commit-callback SPI failure
> handling, arbitrarily large transaction buffering) that substantially exceeds the
> stated 2–3 week estimate once the architectural constraints are respected. The risk
> rating is **Very High** and the SPI-in-change-callback infeasibility makes the
> originally proposed design non-functional. Recommend moving D-2 to a **post-1.0
> research backlog** entirely; do not include it in a numbered milestone until a
> separate feasibility study (outside the release cycle) produces a concrete RFC.

> **D-2 research spike subtotal: ~2–3 weeks**

### Scalability Foundations (pulled forward from v0.13.0)

> **In plain terms:** These items directly serve the project's primary goal of
> world-class performance and scalability. Columnar change tracking eliminates
> wasted delta processing for wide tables, and shared change buffers reduce
> I/O multiplication in deployments with many stream tables reading from the
> same source.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-2 | **Columnar Change Tracking.** Per-column bitmask in CDC triggers; skip rows where no referenced column changed; lightweight UPDATE-only path when only projected columns changed; 50–90% delta-volume reduction for wide-table UPDATE workloads. | 3–4 wk | [PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) |
| D-4 | **Shared Change Buffers.** Single buffer per source shared across all dependent STs; multi-frontier cleanup coordination; static-superset column mode for initial implementation. | 3–4 wk | [PLAN_NEW_STUFF.md §D-4](plans/performance/PLAN_NEW_STUFF.md) |

> **Scalability foundations subtotal: ~6–8 weeks**

### Performance Defaults (from REPORT_OVERALL_STATUS.md)

Targeted improvements identified in the overall status report. None require
large design changes; all build on existing infrastructure.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PERF-2 | **Auto-enable `buffer_partitioning` for high-throughput sources.** `buffer_partitioning` is available but off by default. Add heuristic auto-detection: if a source's change buffer grows beyond `compact_threshold` in less than one scheduler tick interval, automatically switch it to RANGE(lsn) partitioned mode. The `pg_trickle.buffer_partitioning` GUC (default `'off'`) gains an `'auto'` option. Manual override per-source still possible. | 1–2 wk | [REPORT_OVERALL_STATUS.md §R7](plans/performance/REPORT_OVERALL_STATUS.md) |
| PERF-3 | **Flip `tiered_scheduling` default to `true`.** The feature is implemented and tested since v0.10.0. The auto-classification uses a pg_trickle-internal access counter (not `pg_stat_user_tables`) so it is not polluted by internal refresh reads. Changing the default prevents large deployments from wasting CPU refreshing cold STs at full speed indefinitely. Add a CONFIGURATION.md section explaining the Hot/Warm/Cold/Frozen thresholds. | 1–2h | [REPORT_OVERALL_STATUS.md §R11](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-1~~ | ~~**Adaptive scheduler wake interval.**~~ ➡️ Pulled forward to v0.11.0 as WAKE-1. | — | [REPORT_OVERALL_STATUS.md §R3/R16](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-4~~ | ~~**Flip `block_source_ddl` default to `true`.**~~ ➡️ Pulled forward to v0.11.0 as DEF-5. | — | [REPORT_OVERALL_STATUS.md §R12](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-5~~ | ~~**Wider changed-column bitmask (>63 columns).**~~ ➡️ Pulled forward to v0.11.0 as WB-1/WB-2. | — | [REPORT_OVERALL_STATUS.md §R13](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Performance defaults subtotal: ~1–3 weeks**


> **v0.12.0 total: ~18–27 weeks + ~6–8 weeks scalability + ~1–3 weeks defaults + ~3–5 weeks developer tooling & observability**


**Exit criteria:**
- [ ] A-2: Columnar change tracking bitmask skips irrelevant rows; UPDATE-only path reduces delta volume (benchmarked)
- [ ] D-4: Shared buffer serves multiple STs; multi-frontier cleanup prevents premature deletion
- [ ] Fuse triggers on configurable change-count threshold; `reset_fuse()` recovers *(skip if FUSE-1–6 shipped in v0.11.0)*
- [ ] EC01B: No phantom-row drop for ≥3-scan right-subtree joins; TPC-H Q7/Q8/Q9 DELETE regression tests pass
- [ ] BENCH-W: Write-side overhead benchmarks published in `docs/BENCHMARK.md`; `change_buffer_unlogged` GUC implemented if WAL overhead > 30% of trigger cost
- [ ] SQLANCER: Crash-test oracle + equivalence oracle running in weekly CI job; zero correctness mismatches on known test corpus
- [ ] PROP-5+6: Topology stress and DAG/scheduler helper property tests pass
- [ ] D-2 spike: prototype exists; SPI-in-commit-callback constraint validated; RFC written
- [ ] PG 16 and PG 17 pass full E2E suite (trigger CDC mode)
- [ ] WAL decoder validated against PG 16–17 `pgoutput` format
- [ ] CI matrix covers PG 16, 17, 18
- [ ] PERF-2: `buffer_partitioning = 'auto'` implemented; auto-promote benchmark passes
- [ ] PERF-3: `tiered_scheduling` default is `true`; CONFIGURATION.md documents tier thresholds
- [x] ~~PERF-4: `block_source_ddl` default is `true`~~ ➡️ Pulled to v0.11.0 as DEF-5
- [x] ~~PERF-5: Wider bitmask (`BYTEA`) supports >63 columns; schema migration tested~~ ➡️ Pulled to v0.11.0 as WB-1/WB-2
- [ ] DT-1–4: `explain_query_rewrite()`, `diagnose_errors()`, `list_auxiliary_columns()`, `validate_query()` all callable from SQL; each returns structured data
- [ ] G13-SD: Parse-tree visitors enforce `max_parse_depth`; pathological query returns `QueryTooComplex` error rather than stack overflow
- [ ] G17-IMS: IMMEDIATE mode concurrency stress test passes with 100+ concurrent DML transactions
- [ ] G12-SQL-IN: Multi-column IN subquery behavior documented or fixed; regression test added
- [ ] G14-MDED: Deduplication frequency profiling complete; RFC written if compaction threshold exceeded
- [ ] G17-MERGEEX: MERGE template EXPLAIN validation runs at E2E test startup
- [ ] Extension upgrade path tested (`0.11.0 → 0.12.0`)

---

## v0.13.0 — Tiered Scheduling, PG Backward Compatibility & UNLOGGED Buffers

**Goal:** Advance tiered refresh scheduling with manual tier assignment,
widen the deployment target to PG 16–18, and deliver opt-in UNLOGGED change
buffers for reduced WAL amplification.

### Tiered Refresh Scheduling (C-1)

> Items from [PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) Wave 3. Read risk
> analyses before implementing — particularly C-1's read-tracking pitfall.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C-1 | Tiered Refresh Scheduling — Hot/Warm/Cold/Frozen tier classification; lazy refresh for Cold/Frozen STs; configurable per-ST tier override; 80% scheduler-CPU reduction in large deployments | 3–4 wk | [PLAN_NEW_STUFF.md §C-1](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ C-1: Do **not** use raw `pg_stat_user_tables` `seq_scan`/`idx_scan` counters for tier
> classification — pg_trickle's own internal refresh reads inflate these counters, causing
> actively-refreshed-but-unread STs to appear Warm. Use delta-based read tracking or
> expose explicit per-ST tier overrides only. See PLAN_NEW_STUFF.md §C-1 risk analysis.

> **Retraction consideration (C-1):** The auto-classification goal (80% scheduler-CPU
> reduction) cannot be achieved with `pg_stat_user_tables` as the signal. Scope v0.13.0
> to **manual-only tier assignment** (`ALTER STREAM TABLE … SET (tier = 'hot')`) only;
> drop the Hot/Warm/Cold/Frozen auto-classification and the lazy-refresh trigger path.
> Auto-classification requiring a custom `ExecutorStart/End` hook can be revisited
> post-1.0. The effort estimate should drop from 3–4 wk to ~1 wk for the manual-only scope.

> **Tiered scheduling subtotal: ~1–4 weeks**

### PostgreSQL Backward Compatibility (PG 16–18)

> **In plain terms:** pg_trickle currently only targets PostgreSQL 18. This
> work adds support for PG 16 and PG 17 so teams that haven't yet upgraded
> can still use the extension. Each PostgreSQL major version has subtly
> different internal APIs — especially around query parsing and the WAL
> format used for change-data-capture — so each version needs its own
> feature flags, build path, and CI test run.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BC1 | Cargo.toml feature flags (`pg16`, `pg17`, `pg18`) + `cfg_aliases` | 4–8h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 1 |
| BC2 | `#[cfg]` gate JSON_TABLE nodes in `parser.rs` (~250 lines, PG 17+) | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 2 |
| BC3 | `pg_get_viewdef()` trailing-semicolon behavior verification | 2–4h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 3 |
| BC4 | CI matrix expansion (PG 16, 17, 18) + parameterized Dockerfiles | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phases 4–5 |
| BC5 | WAL decoder validation against PG 16–17 `pgoutput` format | 8–12h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §6A |

> **Backward compatibility subtotal: ~38–56 hours**

### UNLOGGED Change Buffers — Opt-In (D-1)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| D-1 | UNLOGGED Change Buffers — create change buffers as `UNLOGGED` to reduce CDC WAL amplification; `pg_trickle.unlogged_buffers` GUC (default `false`, opt-in); crash recovery and standby promotion trigger FULL refresh | 1–2 wk | [PLAN_NEW_STUFF.md §D-1](plans/performance/PLAN_NEW_STUFF.md) |

> Default flipped to `false` (opt-in only) to avoid forced FULL
> refreshes on all stream tables for users who have not explicitly accepted the
> crash/standby tradeoff.

> **D-1 subtotal: ~1–2 weeks**

### Documentation: Best-Practice Patterns Guide (G16-PAT)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G16-PAT | **Best-practice patterns guide.** Add `docs/PATTERNS.md`: bronze/silver/gold materialization strategies, event-sourcing pattern with ST as the projection layer, SCD type-1 and type-2, high-fan-out topology patterns. Each pattern includes worked SQL examples, anti-patterns, and a recommendation on which refresh mode to use. | ~1wk | [plans/performance/REPORT_OVERALL_STATUS.md §16](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Patterns guide subtotal: ~1 week**

### Long-Running Stability & Multi-Database Testing (G17-SOAK, G17-MDB)

> **In plain terms:** By v0.13.0 the core engine is mature enough to warrant
> a 24-hour continuous-integration soak test and a multi-database isolation
> test. Both gaps were identified in the §17 testing analysis.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G17-SOAK | **Long-running stability soak test.** 24h+ continuous mixed DML on 5+ source tables with `DIFFERENTIAL` refresh and Fuse enabled; assert zero worker crashes, zero zombie stream tables, and stable memory usage throughout. | ~1–2d setup | [plans/performance/REPORT_OVERALL_STATUS.md §17](plans/performance/REPORT_OVERALL_STATUS.md) |
| G17-MDB | **Multi-database scheduler isolation test.** Two separate databases in the same PG cluster each with independent pg_trickle background workers; assert that one database's scheduler activity does not interfere with the other's worker quotas or shared-memory state. | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §17](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Stability & multi-database testing subtotal: ~2–4 days**

> **v0.13.0 total: ~9–18 weeks + ~1wk patterns guide + ~2–4 days stability tests**

**Exit criteria:**
- [ ] C-1: Tier classification uses delta-based read tracking; Cold STs skip refresh correctly
- [ ] PG 16 and PG 17 pass full E2E suite (trigger CDC mode)
- [ ] WAL decoder validated against PG 16–17 `pgoutput` format
- [ ] CI matrix covers PG 16, 17, 18
- [ ] D-1: UNLOGGED change buffers opt-in (`unlogged_buffers = false` by default); crash-recovery FULL-refresh path tested
- [ ] G16-PAT: Patterns guide published in `docs/PATTERNS.md` covering at least 4 patterns (bronze/silver/gold, event sourcing, SCD type-1, SCD type-2)
- [ ] G17-SOAK: 24h soak test passes with zero worker crashes, zero zombie stream tables, stable memory usage
- [ ] G17-MDB: Multi-database scheduler isolation verified; no cross-database quota interference
- [ ] Extension upgrade path tested (`0.12.0 → 0.13.0`)

---

## v0.14.0 — Native DDL Syntax, External Test Suites & Integration

**Goal:** Add `CREATE MATERIALIZED VIEW … WITH (pgtrickle.stream = true)` DDL
syntax so stream tables feel native to PostgreSQL tooling (pg_dump, ORMs,
`\dm`), validate correctness against independent query corpora, and ship the
dbt integration as a formal release.

### Native DDL Syntax

> **In plain terms:** Currently you create stream tables by calling a
> function: `SELECT pgtrickle.create_stream_table(...)`. This adds support
> for standard PostgreSQL DDL syntax: `CREATE MATERIALIZED VIEW my_view
> WITH (pgtrickle.stream = true) AS SELECT ...`. That single change means
> `pg_dump` can back them up properly, `\dm` in psql lists them, ORMs can
> introspect them, and migration tools like Flyway treat them like ordinary
> database objects. Stream tables finally look native to PostgreSQL tooling.

Intercept `CREATE/DROP/REFRESH MATERIALIZED VIEW` via `ProcessUtility_hook`
and route stream-table variants through the existing internal implementations.
Allows existing SQL tooling — pg_dump, `\dm`, ORMs — to interact with stream
tables naturally without calling `pgtrickle.create_stream_table()`.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| NAT-1 | `ProcessUtility_hook` infrastructure: register in `_PG_init()`, dispatch+passthrough, hook chaining with TimescaleDB/pg_stat_statements | 3–5d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |
| NAT-2 | CREATE/DROP/REFRESH interception: parse `CreateTableAsStmt` reloptions, route to internal impls, IF EXISTS handling, CONCURRENTLY no-op | 8–13d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |
| NAT-3 | E2E tests: CREATE/DROP/REFRESH via DDL syntax, hook chaining, non-pg_trickle matview passthrough | 2–3d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |

> **Native DDL syntax subtotal: ~13–21 days**

### External Test Suite Integration

> **In plain terms:** pg_trickle's own tests were written by the pg_trickle
> team, which means they can have the same blind spots as the code. This
> adds validation against three independent public benchmarks: PostgreSQL's
> own SQL conformance suite (sqllogictest), the Join Order Benchmark (a
> realistic analytical query workload), and Nexmark (a streaming data
> benchmark). If pg_trickle produces a different answer than PostgreSQL does
> on the same query, these external suites will catch it.

Validate correctness against independent query corpora beyond TPC-H.

> ➡️ **TS1 and TS2 pulled forward to v0.11.0.** Delivering one of TS1 or TS2 is an
> exit criterion for 0.11.0. TS3 (Nexmark) remains in 0.14.0. If TS1/TS2 slip
> from 0.11.0, they land here.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~TS1~~ | ~~sqllogictest: run PostgreSQL sqllogic suite through pg_trickle DIFFERENTIAL mode~~ ➡️ Pulled to v0.11.0 | 2–3d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| ~~TS2~~ | ~~JOB (Join Order Benchmark): correctness baseline and refresh latency profiling~~ ➡️ Pulled to v0.11.0 | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS3 | Nexmark streaming benchmark: sustained high-frequency DML correctness | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |

> **External test suites subtotal: ~1–2 days (TS3 only; TS1/TS2 in v0.11.0)**

### Integration & Release Prep

> **In plain terms:** Ships the dbt integration as a proper
> pip-installable Python package on PyPI so `pip install dbt-pgtrickle`
> works — no manual git cloning required. Alongside that, a full
> documentation review polishes everything so the product is ready to be
> announced to the wider PostgreSQL community.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| I1 | dbt-pgtrickle 0.1.0 formal release (PyPI) | 2–3h | [dbt-pgtrickle/](dbt-pgtrickle/) · [PLAN_DBT_MACRO.md](plans/dbt/PLAN_DBT_MACRO.md) |
| I2 | Complete documentation review & polish | 4–6h | [docs/](docs/) |

> **Integration subtotal: ~6–9 hours**

> **v0.14.0 total: ~140–230 hours**

**Exit criteria:**
- [ ] `CREATE MATERIALIZED VIEW … WITH (pgtrickle.stream = true)` creates a stream table
- [ ] Hook chaining verified with TimescaleDB; non-pgtrickle matviews pass through unchanged
- [ ] At least one external test corpus (sqllogictest, JOB, or Nexmark) passes
- [ ] dbt-pgtrickle 0.1.0 on PyPI
- [ ] Complete documentation review done
- [ ] Extension upgrade path tested (`0.13.0 → 0.14.0`)

---

## v1.0.0 — Stable Release

**Goal:** First officially supported release. Semantic versioning locks in.
API, catalog schema, and GUC names are considered stable. Focus is
distribution — getting pg_trickle onto package registries.

### Release engineering

> **In plain terms:** The 1.0 release is the official "we stand behind this
> API" declaration — from this point on the function names, catalog schema,
> and configuration settings won't change without a major version bump. The
> practical work is getting pg_trickle onto standard package registries
> (PGXN, apt, rpm) so it can be installed with the same commands as any
> other PostgreSQL extension, and hardening the CloudNativePG integration
> for Kubernetes deployments.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R1 | Semantic versioning policy + compatibility guarantees | 2–3h | [PLAN_VERSIONING.md](plans/infra/PLAN_VERSIONING.md) |
| R2 | apt / rpm packaging (Debian/Ubuntu `.deb` + RHEL `.rpm` via PGDG) | 8–12h | [PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) |
| R2b | PGXN `release_status` → `"stable"` (flip one field; PGXN testing release ships in v0.7.0) | 30min | [PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) |
| R3 | ~~Docker Hub official image~~ → CNPG extension image | ✅ Done | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |
| R4 | CNPG operator hardening (K8s 1.33+ native ImageVolume) | 4–6h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |

> **v1.0.0 total: ~18–28 hours**

**Exit criteria:**
- [ ] Published on PGXN (stable) and apt/rpm via PGDG
- [x] CNPG extension image published to GHCR (`pg_trickle-ext`)
- [x] CNPG cluster-example.yaml validated (Image Volume approach)
- [ ] Upgrade path from v0.14.0 tested
- [ ] Semantic versioning policy in effect

---

## Post-1.0 — Scale & Ecosystem

These are not gated on 1.0 but represent the longer-term horizon.

### Ecosystem expansion

> **In plain terms:** Building first-class integrations with the tools most
> data teams already use — a proper dbt adapter (beyond just a
> materialization macro), an Airflow provider so you can trigger stream
> table refreshes from Airflow DAGs, a `pgtrickle` command-line tool for
> managing stream tables without writing SQL, and integration guides for
> popular ORMs and migration frameworks like Django, SQLAlchemy, Flyway, and
> Liquibase.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E1 | dbt full adapter (`dbt-pgtrickle` extending `dbt-postgres`) | 20–30h | [PLAN_DBT_ADAPTER.md](plans/dbt/PLAN_DBT_ADAPTER.md) |
| E2 | Airflow provider (`apache-airflow-providers-pgtrickle`) | 16–20h | [PLAN_ECO_SYSTEM.md §4](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E3 | CLI tool (`pgtrickle`) for management outside SQL | 16–20h | [PLAN_ECO_SYSTEM.md §4](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E4 | Flyway / Liquibase migration support | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E5 | ORM integrations guide (SQLAlchemy, Django, etc.) | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

### Scale

> **In plain terms:** When you have hundreds of stream tables or a very
> large cluster, the single background worker that drives pg_trickle today
> can become a bottleneck. These items explore running the scheduler as an
> external sidecar process (outside the database itself), distributing
> stream tables across Citus shards for horizontal scale-out, and managing
> stream tables that span multiple databases in the same PostgreSQL cluster.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| S1 | External orchestrator sidecar for 100+ STs | 20–40h | [REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md) §D |
| S2 | Citus / distributed PostgreSQL compatibility | ~6 months | [plans/infra/CITUS.md](plans/infra/CITUS.md) |
| S3 | Multi-database support (beyond `postgres` DB) | TBD | [PLAN_MULTI_DATABASE.md](plans/infra/PLAN_MULTI_DATABASE.md) |

### Advanced SQL

> **In plain terms:** A collection of longer-horizon features that each
> require significant research and implementation — full circular dependency
> execution, the remaining pieces of true in-transaction IVM (C-level
> triggers, transition table sharing), backward-compatibility all the way to
> PG 14/15, forward-compatibility with PostgreSQL 19, partitioned stream
> table storage, and several query-planner improvements that reduce the cost
> of computing incremental updates for wide tables and functions with many
> columns.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A2 | Transactional IVM Phase 4 remaining (ENR-based transition tables, aggregate fast-path, C-level triggers, prepared stmt reuse) | ~36–54h | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |
| A3 | PostgreSQL 19 forward-compatibility | TBD | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |
| A4 | PostgreSQL 14–15 backward compatibility | ~40h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) |
| A5 | Partitioned stream table storage (opt-in) | ~60–80h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §4 |
| A6 | Buffer table partitioning by LSN range (`pg_trickle.buffer_partitioning` GUC) | ~3–4d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 4 §3.3 |
| A8 | `ROWS FROM()` with multiple SRF functions — very low demand, deferred | ~1–2d | [PLAN_TRANSACTIONAL_IVM_PART_2.md](plans/sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) Task 2.3 |

### Parser Modularization & Shared Template Cache (G13-PRF, G14-SHC)

> **In plain terms:** Two large-effort research items identified in the deep gap
> analysis. Parser modularization is a prerequisite for native DDL syntax (BC2);
> shared template caching eliminates per-connection cold-start overhead.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G13-PRF | **Modularize `src/dvm/parser.rs`.** At ~19,700 lines (25% of all source), `parser.rs` is too large to maintain safely. Split into sub-modules by SQL construct: `parser/joins.rs`, `parser/aggregates.rs`, `parser/ctes.rs`, `parser/window.rs`, `parser/subqueries.rs`. No behavior change; prerequisite for BC2 (native DDL syntax). | ~3–4wk | [plans/performance/REPORT_OVERALL_STATUS.md §13](plans/performance/REPORT_OVERALL_STATUS.md) |
| G14-SHC | **Shared-memory template caching (research spike).** Evaluate eliminating the per-connection MERGE SQL cold-start (~15–50ms overhead) via PostgreSQL DSM + lwlock. Write an RFC before implementing; validate with a prototype benchmark. | ~2–3wk | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Parser modularization & caching research subtotal: ~5–7 weeks**

### Convenience API Functions (G15-BC, G15-EX)

> **In plain terms:** Two quality-of-life API additions that simplify
> programmatic stream table management, useful for dbt/CI pipelines.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G15-BC | **`bulk_create(definitions JSONB)`** — create multiple stream tables and their CDC triggers in a single transaction. Useful for dbt/CI pipelines that manage many STs programmatically. | ~2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| G15-EX | **`export_definition(name TEXT)`** — export a stream table configuration as reproducible `CREATE STREAM TABLE … WITH (…)` DDL. Useful for backup, versioning, and schema migrations. | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Convenience API subtotal: ~3–5 days**

---

## Effort Summary

| Milestone | Effort estimate | Cumulative | Status |
|-----------|-----------------|------------|--------|
| v0.1.x — Core engine + correctness | ~30h actual | 30h | ✅ Released |
| v0.2.0 — TopK, Diamond & Transactional IVM | ✔️ Complete | 62–78h | ✅ Released |
| v0.2.1 — Upgrade Infrastructure & Documentation | ~8h | 70–86h | ✅ Released |
| v0.2.2 — OFFSET Support, ALTER QUERY & Upgrade Tooling | ~50–70h | 120–156h | ✅ Released |
| v0.2.3 — Non-Determinism, CDC/Mode Gaps & Operational Polish | 45–66h | 165–222h | ✅ Released |
| v0.3.0 — DVM Correctness, SAST & Test Coverage | ~20–30h | 185–252h | ✅ Released |
| v0.4.0 — Parallel Refresh & Performance Hardening | ~60–94h | 245–346h | ✅ Released |
| v0.5.0 — RLS, Operational Controls + Perf Wave 1 (A-3a only) | ~51–97h | 296–443h | ✅ Released |
| v0.6.0 — Partitioning, Idempotent DDL & Circular Dependency Foundation | ~35–50h | 331–493h | ✅ Released |
| v0.7.0 — Performance, Watermarks, Circular DAG Execution, Observability & Infrastructure | ~59–62h | 390–555h | |
| v0.8.0 — pg_dump Support & Test Hardening | ~16–21d | — | |
| v0.9.0 — Incremental Aggregate Maintenance (B-1) | ~7–9 wk | — | |
| v0.10.0 — DVM Hardening, Connection Pooler Compat, Core Refresh Opts & Infra Prep | ~7–10d + ~26–40 wk | — | |
| v0.11.0 — Partitioned Stream Tables, Prometheus & Grafana, Safety Hardening & Correctness | ~7–10 wk + ~12h obs + ~14–21h defaults + ~7–12h safety + ~2–4 wk should-ship | — | |
| v0.12.0 — Scalability Foundations, Anomalous Change Detection & CDC Research | ~18–27 wk + ~6–8 wk scalability + ~1–3 wk defaults | — | |
| v0.13.0 — Tiered Scheduling, PG Backward Compat & UNLOGGED Buffers | ~5–12 wk | — | |
| v0.14.0 — Native DDL Syntax, External Test Suites & Integration | ~140–230h | — | |
| v1.0.0 — Stable release | 18–27h | — | |
| Post-1.0 (ecosystem) | 88–134h | — | |
| Post-1.0 (scale) | 6+ months | — | |

---

## References

| Document | Purpose |
|----------|---------|
| [CHANGELOG.md](CHANGELOG.md) | What's been built |
| [plans/PLAN.md](plans/PLAN.md) | Original 13-phase design plan |
| [plans/sql/SQL_GAPS_7.md](plans/sql/SQL_GAPS_7.md) | 53 known gaps, prioritized |
| [plans/sql/PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md) | Detailed implementation plan for true parallel refresh |
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
