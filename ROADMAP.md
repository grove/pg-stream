# pg_trickle — Project Roadmap

> **Last updated:** 2026-04-10
> **Latest release:** 0.17.0 (2026-04-08)
> **Current milestone:** v0.18.0 — Hardening & Delta Performance

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
- [v0.12.0 — Correctness, Reliability & Developer Tooling](#v0120--correctness-reliability--developer-tooling)
- [v0.13.0 — Scalability Foundations, Partitioning Enhancements, MERGE Profiling & Multi-Tenant Scheduling](#v0130--scalability-foundations-partitioning-enhancements-merge-profiling--multi-tenant-scheduling)
- [v0.14.0 — Tiered Scheduling, UNLOGGED Buffers & Diagnostics](#v0140--tiered-scheduling-unlogged-buffers--diagnostics)
- [v0.15.0 — External Test Suites & Integration](#v0150--external-test-suites--integration)
- [v0.16.0 — Performance & Refresh Optimization](#v0160--performance--refresh-optimization)
- [v0.17.0 — Query Intelligence & Stability](#v0170--query-intelligence--stability)
- [v0.18.0 — Hardening & Delta Performance](#v0180--hardening--delta-performance)
- [v0.19.0 — PGlite Proof of Concept](#v0190--pglite-proof-of-concept)
- [v0.20.0 — Core Extraction (`pg_trickle_core`)](#v0200--core-extraction-pg_trickle_core)
- [v0.21.0 — PGlite WASM Extension](#v0210--pglite-wasm-extension)
- [v0.22.0 — PGlite Reactive Integration](#v0220--pglite-reactive-integration)
- [v1.0.0 — Stable Release](#v100--stable-release)
- [Post-1.0 — Scale, Ecosystem & Platform Expansion](#post-10--scale-ecosystem--platform-expansion)
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

| Version | Theme | Status |
|---------|-------|--------|
| v0.1.x | Core engine, DVM, CDC, scheduling, monitoring | ✅ Released |
| v0.2.0 | TopK, diamond consistency, transactional IVM | ✅ Released |
| v0.2.1 | Upgrade infrastructure & documentation | ✅ Released |
| v0.2.2 | OFFSET, AUTO mode, ALTER QUERY, CDC hardening | ✅ Released |
| v0.2.3 | Non-determinism, CDC/mode gaps, operational polish | ✅ Released |
| v0.3.0 | DVM correctness, SAST & test coverage | ✅ Released |
| v0.4.0 | Parallel refresh & performance hardening | ✅ Released |
| v0.5.0 | Row-level security & operational controls | ✅ Released |
| v0.6.0 | Partitioning, idempotent DDL, circular dependency foundation | ✅ Released |
| v0.7.0 | Performance, watermarks, circular DAG, observability | ✅ Released |
| v0.8.0 | pg_dump support & test hardening | ✅ Released |
| v0.9.0 | Incremental aggregate maintenance | ✅ Released |
| v0.10.0 | DVM hardening, connection pooler compat, refresh optimizations | ✅ Released |
| v0.11.0 | Partitioned stream tables, Prometheus/Grafana, safety hardening | ✅ Released |
| v0.12.0 | Correctness, reliability & developer tooling | ✅ Released |
| v0.13.0 | Scalability foundations, MERGE profiling, multi-tenant scheduling | ✅ Released |
| v0.14.0 | Tiered scheduling, UNLOGGED buffers & diagnostics | ✅ Released |
| v0.15.0 | External test suites & integration | ✅ Released |
| **v0.16.0** | **Performance & refresh optimization** | **✅ Released** |
| **v0.17.0** | **Query intelligence & stability** | **✅ Released** |
| v0.18.0 | Hardening & delta performance | Planned |
| v0.19.0 | PGlite proof of concept | Planned |
| v0.20.0 | Core extraction (`pg_trickle_core`) | Planned |
| v0.21.0 | PGlite WASM extension | Planned |
| v0.22.0 | PGlite reactive integration | Planned |
| v1.0.0 | Stable release (incl. PG 19 compatibility) | Planned |

---

## v0.1.x Series — Released

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

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

</details>

---

## v0.2.1 — Upgrade Infrastructure & Documentation

**Status: Released (2026-03-05).**

Patch release focused on upgrade safety, documentation, and three catalog
schema additions via `sql/pg_trickle--0.2.0--0.2.1.sql`:

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.2.2 — OFFSET, AUTO Mode, ALTER QUERY, Edge Cases & CDC Hardening

**Status: Released (2026-03-08).**

This milestone shipped paged TopK OFFSET support, AUTO-by-default refresh
selection, ALTER QUERY, the remaining upgrade-tooling work, edge-case and WAL
CDC hardening, IMMEDIATE-mode parity fixes, and the outstanding documentation
sweep.

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.2.3 — Non-Determinism, CDC/Mode Gaps & Operational Polish

**Status: Released (2026-03-09).**

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.3.0 — DVM Correctness, SAST & Test Coverage

**Status: Released (2026-03-11).**

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.4.0 — Parallel Refresh & Performance Hardening

**Status: Released (2026-03-12).**

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.5.0 — Row-Level Security & Operational Controls

**Status: Released (2026-03-13).**

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.6.0 — Partitioning, Idempotent DDL, Edge Cases & Circular Dependency Foundation

**Status: Released (2026-03-14).**

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.7.0 — Performance, Watermarks, Circular DAG Execution, Observability & Infrastructure

**Status: Released (2026-03-16).**

**Goal:** Land Part 9 performance improvements (parallel refresh
scheduling, MERGE strategy optimization, advanced benchmarks), add
user-injected temporal watermark gating for batch-ETL coordination,
complete the fixpoint scheduler for circular stream table DAGs, ship
ready-made Prometheus/Grafana monitoring, and prepare the 1.0 packaging
and deployment infrastructure.

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.8.0 — pg_dump Support & Test Hardening

**Status:** Released

**Goal:** Complete the pg_dump round-trip story so stream tables survive
`pg_dump`/`pg_restore` cycles, and comprehensively harden the 
E2E test suites with multiset invariants to mathematically enforce DVM correctness.

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.9.0 — Incremental Aggregate Maintenance

**Status: Released (2026-03-20).**

**Goal:** Implement algebraic incremental maintenance for decomposable aggregates
(COUNT, SUM, AVG, MIN, MAX, STDDEV), reducing per-group refresh from O(group_size)
to O(1) for the common case. This is the highest-potential-payoff item in the
performance plan — benchmarks show aggregate scenarios going from 2.5 ms to sub-1 ms
per group.

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

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

<details>
<summary>Completed items (click to expand)</summary>

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

</details>

---


## v0.11.0 — Partitioned Stream Tables, Prometheus & Grafana Observability, Safety Hardening & Correctness

**Status: Released 2026-03-26.** See [CHANGELOG.md §0.11.0](CHANGELOG.md#0110--2026-03-26) for the full feature list.

**Highlights:** 34× lower latency via event-driven scheduler wake · incremental ST-to-ST
refresh chains · declaratively partitioned stream tables (100× I/O reduction) ·
ready-to-use Prometheus + Grafana monitoring stack · FUSE circuit breaker · VARBIT
changed-column bitmask (no more 63-column cap) · per-database worker quotas ·
DAG scheduling performance improvements (fused chains, adaptive polling, amplification
detection) · TPC-H correctness gate in CI · safer production defaults.

<details>
<summary>Completed items (click to expand)</summary>

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
| ~~C3-1~~ | ~~Per-database worker quotas (`pg_trickle.per_database_worker_quota`); priority ordering (IMMEDIATE > Hot > Warm > Cold); burst capacity up to 150% when other DBs are under budget~~ ✅ Done in v0.11.0 Phase 11 — `compute_per_db_quota()` helper with burst threshold at 80% cluster utilisation; `sort_ready_queue_by_priority()` dispatches ImmediateClosure first; 7 unit tests. | — | [src/scheduler.rs](src/scheduler.rs) |

> **Multi-DB isolation subtotal: ✅ Complete**

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
| G16-GS | **Restructure `GETTING_STARTED.md` with progressive complexity.** Five chapters: (1) Hello World — single-table ST with no join; (2) Multi-table join; (3) Scheduling & backpressure; (4) Monitoring — 5 key functions; (5) Advanced — FUSE, wide bitmask, partitions. Remove the current flat wall-of-SQL structure. ✅ Done in v0.11.0 Phase 11 — 5-chapter structure implemented; Chapter 1 Hello World example added; Chapter 5 Advanced Topics adds inline FUSE, partitioning, IMMEDIATE, and multi-tenant quota examples. | — | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| ~~G16-SM~~ | ~~**SQL/mode operator support matrix.**~~ ✅ Done — 60+ row operator support matrix added to `docs/DVM_OPERATORS.md` covering all operators × FULL/DIFFERENTIAL/IMMEDIATE modes with caveat footnotes. | — | [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) |
| ~~G16-MQR~~ | ~~**Monitoring quick reference.**~~ ✅ Done — Monitoring Quick Reference section added to `docs/GETTING_STARTED.md` with `pgt_status()`, `health_check()`, `change_buffer_sizes()`, `dependency_tree()`, `fuse_status()`, Prometheus/Grafana stack, key metrics table, and alert summary. | — | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| ~~G15-GUC~~ | ~~**GUC interaction matrix.**~~ ✅ Done — GUC Interaction Matrix (14 interaction pairs) and three named Tuning Profiles (Low-Latency, High-Throughput, Resource-Constrained) added to `docs/CONFIGURATION.md`. | — | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |

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

#### Differential ST-to-ST Refresh (✅ Done)

> **In plain terms:** When stream table B's defining query reads from stream
> table A, pg_trickle currently forces a FULL refresh of B every time A
> updates — re-executing B's entire query even when only a handful of rows
> changed. This feature gives ST-to-ST dependencies the same CDC change
> buffer that base tables already have, so B refreshes differentially (applying
> only the delta). Crucially, even when A itself does a FULL refresh, a
> pre/post snapshot diff is captured so B still receives a small I/D delta
> rather than cascading FULL through the chain.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| ST-ST-1 | **Change buffer infrastructure.** `create_st_change_buffer_table()` / `drop_st_change_buffer_table()` in `cdc.rs`; lifecycle hooks in `api.rs`; idempotent `ensure_st_change_buffer()` | ✅ Done | [PLAN_ST_TO_ST.md §Phase 1](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-2 | **Delta capture — DIFFERENTIAL path.** Force explicit DML when ST has downstream consumers; capture delta from `__pgt_delta_{id}` to `changes_pgt_{id}` | ✅ Done | [PLAN_ST_TO_ST.md §Phase 2](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-3 | **Delta capture — FULL path.** Pre/post snapshot diff writes I/D pairs to `changes_pgt_{id}`; eliminates cascading FULL | ✅ Done | [PLAN_ST_TO_ST.md §7](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-4 | **DVM scan operator for ST sources.** Read from `changes_pgt_{id}`; `pgt_`-prefixed LSN tokens; extended frontier and placeholder resolver | ✅ Done | [PLAN_ST_TO_ST.md §Phase 3](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-5 | **Scheduler integration.** Buffer-based change detection in `has_stream_table_source_changes()`; removed FULL override; frontier augmented with ST source positions | ✅ Done | [PLAN_ST_TO_ST.md §Phase 4](plans/sql/PLAN_ST_TO_ST.md) |
| ST-ST-6 | **Cleanup & lifecycle.** `cleanup_st_change_buffers_by_frontier()` for ST buffers; removed prewarm skip for ST sources; ST buffer cleanup in both differential and full refresh paths | ✅ Done | [PLAN_ST_TO_ST.md §Phase 5–6](plans/sql/PLAN_ST_TO_ST.md) |

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
| ~~STRETCH-1~~ | ~~**Partitioned stream tables — design spike only.**~~ ✅ Done in v0.11.0 Partitioning Spike — RFC written ([PLAN_PARTITIONING_SPIKE.md](plans/PLAN_PARTITIONING_SPIKE.md)), go/no-go decision: **Go**. A1-1 implemented (catalog column, API parameter, validation). | 2–4d | [PLAN_PARTITIONING_SPIKE.md](plans/PLAN_PARTITIONING_SPIKE.md) |
| ~~A1-1~~ | ~~**DDL: `CREATE STREAM TABLE … PARTITION BY`; `st_partition_key` catalog column.**~~ ✅ Done — `partition_by` parameter added to all three `create_stream_table*` functions; `st_partition_key TEXT` column in catalog; `validate_partition_key()` validates column exists in output; `build_create_table_sql` emits `PARTITION BY RANGE (key)`; `setup_storage_table` creates default catch-all partition and non-unique `__pgt_row_id` index. | 1–2 wk | [PLAN_PARTITIONING_SPIKE.md](plans/PLAN_PARTITIONING_SPIKE.md) |
| ~~A1-2~~ | ~~**Delta min/max inspection.**~~ ✅ Done — `extract_partition_range()` in `refresh.rs` runs `SELECT MIN/MAX(key)::text` on the resolved delta SQL; returns `None` on empty delta (MERGE skipped). | 1 wk | [PLAN_PARTITIONING_SPIKE.md §8](plans/PLAN_PARTITIONING_SPIKE.md) |
| ~~A1-3~~ | ~~**MERGE rewrite.**~~ ✅ Done — `inject_partition_predicate()` replaces `__PGT_PART_PRED__` placeholder in MERGE ON clause with `AND st."key" BETWEEN 'min' AND 'max'`; `CachedMergeTemplate` stores `delta_sql_template`; D-2 prepared statements disabled for partitioned STs. | 2–3 wk | [PLAN_PARTITIONING_SPIKE.md §8](plans/PLAN_PARTITIONING_SPIKE.md) |
| ~~A1-4~~ | ~~**E2E benchmarks: 10M-row partitioned ST, 0.1%/0.2%/100% change rate scenarios; `EXPLAIN (ANALYZE, BUFFERS)` partition-scan verification.**~~ ✅ Done — 7 E2E tests added to `tests/e2e_partition_tests.rs` covering: initial populate, differential inserts, updates/deletes, empty-delta fast path, EXPLAIN plan verification, invalid partition key rejection; added to light-E2E allowlist. | 1 wk | [PLAN_PARTITIONING_SPIKE.md §9](plans/PLAN_PARTITIONING_SPIKE.md) |

> **Stretch subtotal: STRETCH-1 + A1-1 + A1-2 + A1-3 + A1-4 ✅ All complete**

### DAG Refresh Performance Improvements (from PLAN_DAG_PERFORMANCE.md §8)

> **In plain terms:** Now that ST-to-ST differential refresh eliminates the
> "every hop is FULL" bottleneck, the next performance frontier is reducing
> per-hop overhead and exploiting DAG structure more aggressively. These items
> target the scheduling and dispatch layer — not the DVM engine — and
> collectively can reduce end-to-end propagation latency by 30–50% for
> heterogeneous DAGs.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DAG-1~~ | ~~**Intra-tick pipelining.** Within a single scheduler tick, begin processing a downstream ST as soon as all its specific upstream dependencies have completed — not when the entire topological level finishes. Requires per-ST completion tracking in the parallel dispatch loop and immediate enqueuing of newly-ready STs. Expected 30–50% latency reduction for DAGs with mixed-cost levels.~~ ✅ Done — Already achieved by Phase 4’s parallel dispatch architecture: per-dependency `remaining_upstreams` tracking with immediate downstream readiness propagation. No level barrier exists. 3 validation tests. | 2–3 wk | [PLAN_DAG_PERFORMANCE.md §8.1](plans/performance/PLAN_DAG_PERFORMANCE.md) |
| ~~DAG-2~~ | ~~**Adaptive poll interval.** Replace the fixed 200 ms parallel dispatch poll with exponential backoff (20 ms → 200 ms), resetting on worker completion. Makes parallel mode competitive with CALCULATED for cheap refreshes ($T_r \approx 10\text{ms}$). Alternative: `WaitLatch` with shared-memory completion flags.~~ ✅ Done — `compute_adaptive_poll_ms()` pure-logic helper with exponential backoff (20ms → 200ms); `ParallelDispatchState` tracks `adaptive_poll_ms` + `completions_this_tick`; resets to 20ms on worker completion; 8 unit tests. | 1–2 wk | [PLAN_DAG_PERFORMANCE.md §8.2](plans/performance/PLAN_DAG_PERFORMANCE.md) |
| ~~DAG-3~~ | ~~**Delta amplification detection.** Track input→output delta ratio per hop via `pgt_refresh_history`. When a join ST amplifies delta beyond a configurable threshold (e.g., output > 100× input), emit a performance WARNING and optionally fall back to FULL for that hop. Expose amplification metrics in `explain_st()`.~~ ✅ Done — `pg_trickle.delta_amplification_threshold` GUC (default 100×); `compute_amplification_ratio` + `should_warn_amplification` pure-logic helpers; WARNING emitted after MERGE with ratio, counts, and tuning hint; `explain_st()` exposes `amplification_stats` JSON from last 20 DIFFERENTIAL refreshes; 15 unit tests. | 3–5d | [PLAN_DAG_PERFORMANCE.md §8.4](plans/performance/PLAN_DAG_PERFORMANCE.md) |
| ~~DAG-4~~ | ~~**ST buffer bypass for single-consumer CALCULATED chains.** For ST dependencies with exactly one downstream consumer refreshing in the same tick, pass the delta in-memory instead of writing/reading from the `changes_pgt_` buffer table. Eliminates 2× SPI DML per hop (~20 ms savings per hop for 10K-row deltas).~~ ✅ Done — `FusedChain` execution unit kind; `find_fusable_chains()` pure-logic detection; `capture_delta_to_bypass_table()` writes to temp table; `DiffContext.st_bypass_tables` threads bypass through DVM scan; delta SQL cache bypassed when active; 11+4 unit tests. | 3–4 wk | [PLAN_DAG_PERFORMANCE.md §8.3](plans/performance/PLAN_DAG_PERFORMANCE.md) |
| ~~DAG-5~~ | ~~**ST buffer batch coalescing.** Apply net-effect computation to ST change buffers before downstream reads — cancel INSERT/DELETE pairs for the same `__pgt_row_id` that accumulate between reads during rapid-fire upstream refreshes. Adapts existing `compute_net_effect()` logic to the ST buffer schema.~~ ✅ Done — `compact_st_change_buffer()` with `build_st_compact_sql()` pure-logic helper; advisory lock namespace 0x5047_5500; integrated in `execute_differential_refresh()` after C-4 base-table compaction; 9 unit tests. | 1–2 wk | [PLAN_DAG_PERFORMANCE.md §8.5](plans/performance/PLAN_DAG_PERFORMANCE.md) |

> **DAG refresh performance subtotal: ~8–12 weeks**

> **v0.11.0 total: ~7–10 weeks (partitioning + isolation) + ~12h observability + ~14–21h default tuning + ~7–12h safety hardening + ~2–4 weeks should-ship (bitmask + fuse + external corpus) + ~4.5–6.5 weeks ST-to-ST differential + ~2–3 weeks event-driven wake + ~1–2 days correctness quick-wins + ~2–3 days documentation + ~8–12 weeks DAG performance**

**Exit criteria: ✅ All met. Released 2026-03-26.**
- [x] Declaratively partitioned stream tables accepted; partition key tracked in catalog — ✅ Done in v0.11.0 Partitioning Spike (STRETCH-1 RFC + A1-1)
- [x] Partitioned storage table created with `PARTITION BY RANGE` + default catch-all partition — ✅ Done (A1-1 physical DDL)
- [x] Partition-key range predicate injected into MERGE ON clause; empty-delta fast-path skips MERGE — ✅ Done (A1-2 + A1-3)
- [x] Partition-scoped MERGE benchmark: 10M-row ST, 0.1% change rate (expect ~100× I/O reduction) — ✅ Done (A1-4 E2E tests)
- [x] Per-database worker quotas enforced; burst reclaimed within 1 scheduler cycle — ✅ Done in v0.11.0 Phase 11 (`pg_trickle.per_database_worker_quota` GUC; burst to 150% at < 80% cluster load)
- [x] Prometheus queries + alerting rules + Grafana dashboard shipped — ✅ Done in v0.11.0 Phase 3 (`monitoring/` directory)
- [x] DEF-1: `parallel_refresh_mode` default is `'on'`; unit test updated — ✅ Done in v0.11.0 Phase 1
- [x] DEF-2: `auto_backoff` default is `true`; CONFIGURATION.md updated — ✅ Done in v0.10.0
- [x] DEF-3: SemiJoin delta-key pre-filter verified already implemented — ✅ Done in v0.11.0 Phase 2 (pre-existing in `semi_join.rs`)
- [x] DEF-4: Invalidation ring capacity is 128 slots — ✅ Done in v0.11.0 Phase 1
- [x] DEF-5: `block_source_ddl` default is `true`; error message includes escape-hatch instructions — ✅ Done in v0.11.0 Phase 1
- [x] SAF-1: No `panic!`/`unwrap()` in background worker hot paths; `check_skip_needed` logs SPI errors — ✅ Done in v0.11.0 Phase 1
- [x] SAF-2: Failure-injection E2E tests in `tests/e2e_safety_tests.rs` — ✅ Done in v0.11.0 Phase 2
- [x] WB-1+2: Changed-column bitmask supports >63 columns (VARBIT); wide-table CDC selectivity E2E passes; schema migration tested — ✅ Done in v0.11.0 Phase 5
- [x] FUSE-1–6: Fuse blows on configurable change-count threshold; `reset_fuse()` recovers in all three action modes; diamond/DAG interaction tested — ✅ Done in v0.11.0 Phase 6
- [x] TS2: TPC-H-derived 5-query DIFFERENTIAL correctness gate passes with zero mismatches; gated in CI — ✅ Done in v0.11.0 Phase 9
- [x] QF-1–4: `println!` replaced with guarded `pgrx::log!()`; AUTO downgrades emit `WARNING`; `append_only` reversion verified already warns; parser invariant sites annotated — ✅ Done in v0.11.0 Phase 1
- [x] G12-ERM: `effective_refresh_mode` column present in `pgt_stream_tables`; `explain_refresh_mode()` returns configured mode, effective mode, downgrade reason — ✅ Done in v0.11.0 Phase 2
- [x] G12-2: TopK path validates assumptions at refresh time; triggers FULL fallback with `WARNING` on violation — ✅ Done in v0.11.0 Phase 4
- [x] G12-AGG: Group-rescan aggregate warning fires at `create_stream_table` for DIFFERENTIAL mode; strategy visible in `explain_st()` — ✅ Done in v0.11.0 Phase 4
- [x] G15-PV: Incompatible `cdc_mode`/`refresh_mode` and `diamond_schedule_policy` combinations rejected at creation time with structured `HINT` — ✅ Done in v0.11.0 Phase 2
- [x] G13-EH: `UnsupportedOperator`, `CycleDetected`, `UpstreamSchemaChanged`, `QueryParseError` include `DETAIL` and `HINT` fields — ✅ Done in v0.11.0 Phase 2
- [x] G17-EC01B-NEG: Negative regression test documents ≥3-scan fall-back behavior; linked to v0.12.0 EC01B fix — ✅ Done in v0.11.0 Phase 4
- [x] G16-GS/SM/MQR/GUC: GETTING_STARTED restructured (5 chapters + Hello World + Advanced Topics); DVM_OPERATORS support matrix; monitoring quick reference; CONFIGURATION.md GUC matrix — ✅ Done in v0.11.0 Phase 11
- [x] ST-ST-1–6: All ST-to-ST dependencies refresh differentially when upstream has a change buffer; FULL refreshes on upstream produce pre/post I/D diff; no cascading FULL — ✅ Done in v0.11.0 Phase 8
- [x] WAKE-1: Event-driven scheduler wake; median latency ~15 ms (34× improvement); 10 ms debounce; poll fallback — ✅ Done in v0.11.0 Phase 7
- [x] DAG-1: Intra-tick pipelining confirmed in Phase 4 architecture — ✅ Done
- [x] DAG-2: Adaptive poll interval (20 ms → 200 ms exponential backoff) — ✅ Done in v0.11.0 Phase 10
- [x] DAG-3: Delta amplification detection with `pg_trickle.delta_amplification_threshold` GUC — ✅ Done in v0.11.0 Phase 10
- [x] DAG-4: ST buffer bypass (`FusedChain`) for single-consumer CALCULATED chains — ✅ Done in v0.11.0 Phase 10
- [x] DAG-5: ST buffer batch coalescing cancels redundant I/D pairs — ✅ Done in v0.11.0 Phase 10
- [x] Extension upgrade path tested (`0.10.0 → 0.11.0`) — ✅ upgrade SQL in `sql/pg_trickle--0.10.0--0.11.0.sql`

</details>

---


## v0.12.0 — Correctness, Reliability & Developer Tooling

**Goal:** Close the last known wrong-answer bugs in the incremental query
engine, add SQL-callable diagnostic functions for observability, harden the
scheduler against edge cases uncovered with deeper topologies, and back the
whole release with thousands of automatically generated property and fuzz tests.

Phases 5–8 from the original v0.12.0 scope (Scalability Foundations,
Partitioning Enhancements, MERGE Profiling, and dbt Macro Updates) have been
**moved to v0.13.0** to keep this release tightly focused on correctness and
reliability. See §v0.13.0 for those items.

**Status: Released (2026-03-28).**


<details>
<summary>Completed items (click to expand)</summary>

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
| ~~EC01B-1~~ | ~~Design and implement a per-subtree CTE-based snapshot strategy to replace EXCEPT ALL for right-side join chains with ≥3 scan nodes; remove the `join_scan_count(child) <= 2` threshold in `use_pre_change_snapshot`~~ ✅ Done | — | [src/dvm/operators/join_common.rs](src/dvm/operators/join_common.rs) · [plans/PLAN_EDGE_CASES.md §EC-01](plans/PLAN_EDGE_CASES.md) |
| ~~EC01B-2~~ | ~~TPC-H Q7/Q8/Q9 regression tests: combined left-DELETE + right-DELETE in same cycle; assert no phantom-row drop~~ ✅ Done | — | [tests/e2e_tpch_tests.rs](tests/e2e_tpch_tests.rs) |

> **EC-01 deep fix subtotal: ~3–4 weeks — ✅ Complete**

### CDC Write-Side Overhead Benchmark

> **In plain terms:** Every INSERT/UPDATE/DELETE on a source table fires a PL/pgSQL
> trigger that writes to the change buffer. We have never measured how much write
> throughput this costs. These benchmarks quantify it across five scenarios (single-row,
> bulk INSERT, bulk UPDATE, bulk DELETE, concurrent writers) and gate the decision on
> whether to implement a `change_buffer_unlogged` GUC that could reduce WAL overhead
> by ~20–30%.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~BENCH-W1~~ | ~~Implement `tests/e2e_cdc_write_overhead_tests.rs`: compare source-only vs. source + stream table DML throughput across five scenarios; report write amplification factor~~ ✅ Done | — | [tests/e2e_cdc_write_overhead_tests.rs](tests/e2e_cdc_write_overhead_tests.rs) |
| ~~BENCH-W2~~ | ~~Publish results in `docs/BENCHMARK.md`~~ ✅ Done | — | [docs/BENCHMARK.md](docs/BENCHMARK.md) |

> **CDC write-side benchmark subtotal: ~3–5 days — ✅ Complete**

### DAG Topology Benchmark Suite (from PLAN_DAG_BENCHMARK.md)

> **In plain terms:** Production deployments form DAGs with 10–500+ stream
> tables arranged in chains, fan-outs, diamonds, and mixed topologies. This
> benchmark suite measures end-to-end propagation latency and throughput
> through these DAG shapes, validates the theoretical latency formulas from
> PLAN_DAG_PERFORMANCE.md, and provides regression detection for DAG
> propagation overhead.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DAG-B1~~ | ~~Session 1: Infrastructure, linear chain topology builder, latency + throughput measurement drivers, reporting (ASCII/JSON), 7 benchmark tests~~ ✅ Done | — | [PLAN_DAG_BENCHMARK.md](plans/performance/PLAN_DAG_BENCHMARK.md) §11.1 |
| ~~DAG-B2~~ | ~~Session 2: Wide DAG + fan-out tree topology builders; 9 latency + throughput tests (5 wide + 2 fan-out latency, 2 throughput)~~ ✅ Done | — | [PLAN_DAG_BENCHMARK.md](plans/performance/PLAN_DAG_BENCHMARK.md) §11.2 |
| ~~DAG-B3~~ | ~~Session 3: Diamond + mixed topology builders; 5 latency + throughput tests; per-level breakdown reporting~~ ✅ Done | — | [PLAN_DAG_BENCHMARK.md](plans/performance/PLAN_DAG_BENCHMARK.md) §11.3 |
| ~~DAG-B4~~ | ~~Session 4: Update `docs/BENCHMARK.md`, full suite validation run~~ ✅ Done | — | [PLAN_DAG_BENCHMARK.md](plans/performance/PLAN_DAG_BENCHMARK.md) §11.4 |

> **DAG topology benchmark subtotal: ~3–5 days — ✅ Complete**

### Developer Tooling & Observability Functions (from REPORT_OVERALL_STATUS.md §15) ✅ Complete

> **In plain terms:** pg_trickle's diagnostic toolbox today is limited to
> `explain_st()` and `refresh_history()`. Operators debugging unexpected mode
> changes, query rewrites, or error patterns must read source code or server
> logs. This section adds four SQL-callable diagnostic functions that surface
> internal state in a structured, queryable form.

| Item | Description | Effort | Status |
|------|-------------|--------|--------|
| DT-1 | **`explain_query_rewrite(query TEXT)`** — parse a query through the DVM pipeline and return the rewritten SQL plus a list of passes applied (operator rewrites, delta-key injections, TopK detection, group-rescan classification). Useful for debugging unexpected refresh behavior without creating a stream table. | ~1–2d | ✅ Done in v0.12.0 Phase 2 |
| DT-2 | **`diagnose_errors(name TEXT)`** — return the last 5 error events for a stream table, classified by type (correctness, performance, config, infrastructure), with a suggested remediation for each class. | ~2–3d | ✅ Done in v0.12.0 Phase 2 |
| DT-3 | **`list_auxiliary_columns(name TEXT)`** — list all `__pgt_*` internal columns injected into the stream table's query plan with their purpose (delta tracking, row identity, compaction key). Helps users understand unexpected columns in `SELECT *` output. | ~1d | ✅ Done in v0.12.0 Phase 2 |
| DT-4 | **`validate_query(query TEXT)`** — parse and run DVM validation on a query without creating a stream table; return the resolved refresh mode, detected SQL constructs (group-rescan aggregates, non-equijoins, multi-scan subtrees), and any warnings. | ~1–2d | ✅ Done in v0.12.0 Phase 2 |

> **Developer tooling subtotal: ~5–8 days**

### Parser Safety, Concurrency & Query Coverage (from REPORT_OVERALL_STATUS.md §13/§12/§17)

> Additional correctness and robustness items from the deep gap analysis:
> a stack-overflow prevention guard for pathological queries, a concurrency
> stress test for IMMEDIATE mode, and two investigations into known under-
> documented query constructs.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G13-SD~~ | ~~**Parser recursion depth limit.** Add a recursion depth counter to all recursive parse-tree visitor functions in `dvm/parser.rs`. Return `PgTrickleError::QueryTooComplex` if depth exceeds `pg_trickle.max_parse_depth` (GUC, default 64). Prevents stack-overflow crashes on pathological queries.~~ ✅ Done | — | [src/dvm/parser.rs](src/dvm/parser.rs) · [src/config.rs](src/config.rs) · [src/error.rs](src/error.rs) |
| ~~G17-IMS~~ | ~~**IMMEDIATE mode concurrency stress test.** 100+ concurrent DML transactions on the same source table in `IMMEDIATE` refresh mode; assert zero lost updates, zero phantom rows, and no deadlocks.~~ ✅ Done | — | [tests/e2e_immediate_concurrency_tests.rs](tests/e2e_immediate_concurrency_tests.rs) |
| ~~G12-SQL-IN~~ | ~~**Multi-column `IN (subquery)` correctness investigation.** Determine behavior when DVM encounters `EXPR IN (subquery returning multiple columns)`. Add a correctness test; if the construct is broken, fix it or document as unsupported with a structured error.~~ ✅ Done — documented as unsupported | — | [tests/e2e_multi_column_in_tests.rs](tests/e2e_multi_column_in_tests.rs) · [src/dvm/parser.rs](src/dvm/parser.rs) |
| G14-MDED | **MERGE deduplication profiling.** Profile how often concurrent-write scenarios produce duplicate key entries requiring pre-MERGE compaction. If ≥10% of refresh cycles need dedup, write an RFC for a two-pass MERGE strategy. | ~3–5d | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~G17-MERGEEX~~ | ~~**MERGE template EXPLAIN validation in E2E tests.** Add `EXPLAIN (COSTS OFF)` dry-run checks for generated MERGE SQL templates at E2E test startup. Catches malformed templates before any data is processed.~~ ✅ Done | — | [tests/e2e_merge_template_tests.rs](tests/e2e_merge_template_tests.rs) |

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
| SQLANCER-1 | Docker-based harness: `just sqlancer` spins up E2E container; crash-test oracle verifies that no SQLancer-generated `create_stream_table` call crashes the backend | 3–4d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §Steps 1–2 | ✅ Done in v0.12.0 Phase 4 |
| SQLANCER-2 | Equivalence oracle: for each generated query Q, assert `create_stream_table` + `refresh` output equals native `SELECT` (multiset comparison); failures auto-committed as proptest regression seeds | 3–4d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §Step 3 | ✅ Done in v0.12.0 Phase 4 |
| SQLANCER-3 | CI `weekly-sqlancer` job (daily schedule + manual dispatch); new proptest seed files committed on any detected correctness failure | 1–2d | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) | ✅ Done in v0.12.0 Phase 4 |

> **SQLancer fuzzing subtotal: ~1–2 weeks**

### Property-Based Invariant Tests (Items 5 & 6)

> **In plain terms:** Items 1–4 of the property test plan are done. These two
> remaining items add topology/scheduler stress tests (random DAG shapes with
> multi-source branch interactions) and pure Rust unit-level properties (ordering
> monotonicity, SCC bookkeeping correctness). Both slot into the existing proptest
> harness and provide coverage that example-based tests cannot exhaustively explore.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PROP-5 | Topology / scheduler stress: randomized DAG topologies with multi-source branch interactions; assert no incorrect refresh ordering or spurious suspension | 4–6d | [PLAN_TEST_PROPERTY_BASED_INVARIANTS.md](plans/testing/PLAN_TEST_PROPERTY_BASED_INVARIANTS.md) §Item 5 | ✅ Done in v0.12.0 Phase 4 |
| PROP-6 | Pure Rust DAG / scheduler helper properties: ordering invariants, monotonic metadata helpers, SCC bookkeeping edge-cases | 2–4d | [PLAN_TEST_PROPERTY_BASED_INVARIANTS.md](plans/testing/PLAN_TEST_PROPERTY_BASED_INVARIANTS.md) §Item 6 | ✅ Done in v0.12.0 Phase 4 |

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
| A-2 | **Columnar Change Tracking.** Per-column bitmask in CDC triggers; skip rows where no referenced column changed; lightweight UPDATE-only path when only projected columns changed; 50–90% delta-volume reduction for wide-table UPDATE workloads. | 3–4 wk | [PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) | ✅ Done |
| D-4 | **Shared Change Buffers.** Single buffer per source shared across all dependent STs; multi-frontier cleanup coordination; static-superset column mode for initial implementation. | 3–4 wk | [PLAN_NEW_STUFF.md §D-4](plans/performance/PLAN_NEW_STUFF.md) | ✅ Done |

> **Scalability foundations subtotal: ~6–8 weeks**

### Partitioning Enhancements (A1 follow-ons from v0.11.0 spike)

> **In plain terms:** The v0.11.0 spike delivered RANGE partitioning end-to-end.
> These follow-on items extend coverage to the use cases deliberately deferred
> from A1: multi-column keys, retrofitting existing stream tables, LIST-based
> partitions, HASH partitions (which need a different strategy than predicate
> injection), and operational quality-of-life improvements.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~A1-1b~~ | ~~**Multi-column partition keys.** Comma-separated `partition_by`; `PARTITION BY RANGE (col_a, col_b)`; multi-column MIN/MAX extraction; ROW() comparison predicates for partition pruning.~~ ✅ Done — `parse_partition_key_columns()`, composite `extract_partition_range()`, ROW comparison in `inject_partition_predicate()`; 5 unit tests + 3 E2E tests | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~A1-1c~~ | ~~**`alter_stream_table(partition_by => …)` support.** Add/change/remove partition key on existing stream tables; `alter_stream_table_partition_key()` handles DROP + recreate + full refresh; `update_partition_key()` in catalog; SQL migration adds parameter; also fixed `alter_stream_table_query` to preserve partition key.~~ ✅ Done — 4 E2E tests | — | [src/api.rs](src/api.rs), [src/catalog.rs](src/catalog.rs) |
| ~~A1-1d~~ | ~~**LIST partitioning support.** `partition_by => 'LIST:col'` creates `PARTITION BY LIST` storage; `PartitionMethod` enum dispatches LIST vs RANGE; `extract_partition_bounds()` uses `SELECT DISTINCT` for LIST; `inject_partition_predicate()` emits `IN (…)` predicate; single-column-only validation.~~ ✅ Done — 16 unit tests + 4 E2E tests | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~A1-3b~~ | ~~**HASH partitioning via per-partition MERGE loop.** `partition_by => 'HASH:col[:N]'` creates `PARTITION BY HASH` storage with N auto-created child partitions; `execute_hash_partitioned_merge()` materializes delta → discovers children via `pg_inherits` → per-child MERGE filtered through `satisfies_hash_partition()`; `build_hash_child_merge()` rewrites MERGE targeting `ONLY child_partition`.~~ ✅ Done — 22 unit tests + 6 E2E tests | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~PART-WARN~~ | ~~**Default-partition growth warning.** `warn_default_partition_growth()` emits `pgrx::warning!()` after FULL and DIFFERENTIAL refresh when the default partition has rows; includes example DDL.~~ ✅ Done — 2 E2E tests | — | [src/refresh.rs](src/refresh.rs) |

> **Auto-partition creation** (TimescaleDB-style automatic chunk management) remains
> a post-1.0 item as stated in PLAN_PARTITIONING_SPIKE.md §10.

> **Partitioning enhancements subtotal: ~5–8 weeks**

### Performance Defaults (from REPORT_OVERALL_STATUS.md)

Targeted improvements identified in the overall status report. None require
large design changes; all build on existing infrastructure.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~PERF-2~~ | ~~**Auto-enable `buffer_partitioning` for high-throughput sources.**~~ ✅ Done — `should_promote_inner()` throughput-based heuristic; `convert_buffer_to_partitioned()` runtime migration; auto-promote hook in `execute_differential_refresh()`; `docs/CONFIGURATION.md` updated; 10 unit tests + 3 E2E tests | — | [REPORT_OVERALL_STATUS.md §R7](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-3~~ | ~~**Flip `tiered_scheduling` default to `true`.** The feature is implemented and tested since v0.10.0.~~ ✅ Done — default flipped; CONFIGURATION.md updated with tier thresholds section | — | [src/config.rs](src/config.rs) · [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| ~~PERF-1~~ | ~~**Adaptive scheduler wake interval.**~~ ➡️ Pulled forward to v0.11.0 as WAKE-1. | — | [REPORT_OVERALL_STATUS.md §R3/R16](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-4~~ | ~~**Flip `block_source_ddl` default to `true`.**~~ ➡️ Pulled forward to v0.11.0 as DEF-5. | — | [REPORT_OVERALL_STATUS.md §R12](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~PERF-5~~ | ~~**Wider changed-column bitmask (>63 columns).**~~ ➡️ Pulled forward to v0.11.0 as WB-1/WB-2. | — | [REPORT_OVERALL_STATUS.md §R13](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Performance defaults subtotal: ~1–3 weeks**

### DAG Refresh Performance Improvements (from PLAN_DAG_PERFORMANCE.md §8)

> ➡️ **Moved to v0.11.0** — these items build directly on the ST-to-ST
> differential infrastructure shipped in v0.11.0 Phase 8 and are most
> impactful while that work is fresh.


> **v0.12.0 total: ~18–27 weeks + ~6–8 weeks scalability + ~5–8 weeks partitioning enhancements + ~1–3 weeks defaults + ~3–5 weeks developer tooling & observability**

> **Priority tiers:** P0 = Phases 1–3 (must ship); P1 = Phases 4 + 7 (target); P2 = Phases 5, 6, 8 (can defer to v0.13.0 as a unit — never partially ship Phase 5/6).

### dbt Macro Updates (Phase 8)

> **Priority P2** — Expose the v0.11.0 SQL API additions (`partition_by`, `fuse`,
> `fuse_ceiling`, `fuse_sensitivity`) in the dbt materialization macros so
> dbt users can configure them via `config(...)`. No catalog changes; pure
> Jinja/SQL. Can defer to v0.13.0 as a unit.

| Item | Description | Effort |
|------|-------------|--------|
| DBT-1 | `partition_by` config option wired through `stream_table.sql`, `create_stream_table.sql`, and `alter_stream_table.sql` | ~1d |
| DBT-2 | `fuse`, `fuse_ceiling`, `fuse_sensitivity` config options wired through the materialization and alter macro with change-detection logic | ~1–2d |
| DBT-3 | dbt docs update: README and SQL_REFERENCE.md dbt section | ~0.5d |

> **dbt macro updates subtotal: ~2–3.5 days**


**Exit criteria — all met (v0.12.0 Released 2026-03-28):**
- [x] EC01B-1/2: No phantom-row drop for ≥3-scan right-subtree joins; TPC-H Q7/Q8/Q9 DELETE regression tests pass ✅
- [x] BENCH-W: Write-side overhead benchmarks published in `docs/BENCHMARK.md` ✅
- [x] DAG-B1–B4: DAG topology benchmark suite complete ✅
- [x] SQLANCER-1/2/3: Crash-test + equivalence oracles in weekly CI job; zero mismatches ✅
- [x] PROP-5+6: Topology stress and DAG/scheduler helper property tests pass ✅
- [x] DT-1–4: `explain_query_rewrite()`, `diagnose_errors()`, `list_auxiliary_columns()`, `validate_query()` callable from SQL ✅
- [x] G13-SD: `max_parse_depth` guard active; pathological query returns `QueryTooComplex` ✅
- [x] G17-IMS: IMMEDIATE mode concurrency stress test (5 scenarios × 100+ concurrent DML) passes ✅
- [x] G12-SQL-IN: Multi-column IN subquery documented as unsupported with structured error + EXISTS hint ✅
- [x] G17-MERGEEX: MERGE template EXPLAIN validation at E2E test startup ✅
- [x] PERF-3: `tiered_scheduling` default is `true`; CONFIGURATION.md updated ✅
- [x] ST-ST-9: Content-hash pk_hash in ST change buffers; stale-row-after-UPDATE bug fixed ✅
- [x] DAG-4 bypass column types fixed; parallel worker tests complete without timeout ✅
- [x] `docs/UPGRADING.md` updated with v0.11.0→v0.12.0 migration notes ✅
- [x] `scripts/check_upgrade_completeness.sh` passes ✅
- [x] Extension upgrade path tested (`0.11.0 → 0.12.0`) ✅

</details>

---


## v0.13.0 — Scalability Foundations, Partitioning Enhancements, MERGE Profiling & Multi-Tenant Scheduling

**Status: Released (2026-03-31).**

**Goal:** Deliver the scalability foundations deferred from v0.12.0 —
columnar change tracking and shared change buffers — alongside the
partitioning enhancements that build on v0.11.0's RANGE partitioning spike,
a MERGE deduplication profiling pass, the dbt macro updates, per-database
worker quotas for multi-tenant deployments, the TPC-H-derived benchmarking
harness for data-driven performance validation, and a small SQL coverage
cleanup for PG 16+ expression types.

<details>
<summary>Completed items (click to expand)</summary>

> **Phases from PLAN_0_12_0.md:** Phases 5 (Scalability), 6 (Partitioning),
> 7 (MERGE Profiling), and 8 (dbt Macro Updates). Plus three new phases: 9
> (Multi-Tenant Scheduler Isolation), 10 (TPC-H Benchmark Harness), and 11
> (SQL Coverage Cleanup).

### Scalability Foundations (Phase 5)

> **In plain terms:** These items directly serve the project's primary goal of
> world-class performance and scalability. Columnar change tracking eliminates
> wasted delta processing for wide tables, and shared change buffers reduce
> I/O multiplication in deployments with many stream tables reading from the
> same source.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-2 | **Columnar Change Tracking.** Per-column bitmask in CDC triggers; skip rows where no referenced column changed; lightweight UPDATE-only path when only projected columns changed; 50–90% delta-volume reduction for wide-table UPDATE workloads. | 3–4 wk | [PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) | ✅ Done |
| D-4 | **Shared Change Buffers.** Single buffer per source shared across all dependent STs; multi-frontier cleanup coordination; static-superset column mode for initial implementation. | 3–4 wk | [PLAN_NEW_STUFF.md §D-4](plans/performance/PLAN_NEW_STUFF.md) | ✅ Done |
| ~~PERF-2~~ | ~~**Auto-enable `buffer_partitioning` for high-throughput sources.**~~ ✅ Done — throughput-based auto-promotion: buffer exceeding `compact_threshold` in a single refresh cycle is converted to RANGE(lsn) partitioned mode at runtime. | — | [REPORT_OVERALL_STATUS.md §R7](plans/performance/REPORT_OVERALL_STATUS.md) |

> ⚠️ D-4 **multi-frontier cleanup correctness verified.** `MIN(consumer_frontier)`
> used in all cleanup paths. Property-based tests with 5–10 consumers and
> 500 random frontier advancement cases pass.

> **Scalability foundations subtotal: ~6–8 weeks**

### Partitioning Enhancements (Phase 6)

> **In plain terms:** The v0.11.0 spike delivered RANGE partitioning end-to-end.
> These follow-on items extend coverage to the use cases deliberately deferred
> from A1: multi-column keys, retrofitting existing stream tables, LIST-based
> partitions, HASH partitions, and operational quality-of-life improvements.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~A1-1b~~ | ~~**Multi-column partition keys.** Comma-separated `partition_by`; ROW() predicate for composite keys.~~ ✅ Done | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~A1-1c~~ | ~~**`alter_stream_table(partition_by => …)` support.** Add/change/remove partition key with full storage rebuild.~~ ✅ Done | — | [src/api.rs](src/api.rs), [src/catalog.rs](src/catalog.rs) |
| ~~A1-1d~~ | ~~**LIST partitioning support.** `PARTITION BY LIST` for low-cardinality columns; `IN (…)` predicate style from the delta.~~ ✅ Done | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~A1-3b~~ | ~~**HASH partitioning via per-partition MERGE loop.** `HASH:col[:N]` with auto-created child partitions; per-partition MERGE through `satisfies_hash_partition()`.~~ ✅ Done | — | [src/api.rs](src/api.rs), [src/refresh.rs](src/refresh.rs) |
| ~~PART-WARN~~ | ~~**Default-partition growth warning.** `warn_default_partition_growth()` after FULL and DIFFERENTIAL refresh.~~ ✅ Done | — | [src/refresh.rs](src/refresh.rs) |

> **Partitioning enhancements subtotal: ~5–8 weeks**

### MERGE Profiling (Phase 7)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G14-MDED | **MERGE deduplication profiling.** Profile how often concurrent-write scenarios produce duplicate key entries requiring pre-MERGE compaction. If ≥10% of refresh cycles need dedup, write an RFC for a two-pass MERGE strategy. | 3–5d | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |
| PROF-DLT | **Delta SQL query plan profiling (`explain_delta()` function).** Capture `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` for auto-generated delta SQL queries to identify PostgreSQL execution bottlenecks (join algorithms, scan types, sort spills). Add `pgtrickle.explain_delta(st_name, format DEFAULT 'text')` SQL function; optional `PGS_PROFILE_DELTA=1` environment variable for E2E test auto-capture to `/tmp/delta_plans/<st>.json`. Enables identification of operator-level performance issues (semi-join full scans, deep join chains). Prerequisite for data-driven MERGE optimization. | 1–2w | [PLAN_TPC_H_BENCHMARKING.md §1-5](plans/performance/PLAN_TPC_H_BENCHMARKING.md) |

> **MERGE profiling subtotal: ~1–3 weeks**

### dbt Macro Updates (Phase 8)

> **In plain terms:** Expose the v0.11.0 SQL API additions (`partition_by`, `fuse`,
> `fuse_ceiling`, `fuse_sensitivity`) in the dbt materialization macros so
> dbt users can configure them via `config(...)`. No catalog changes; pure
> Jinja/SQL.

| Item | Description | Effort |
|------|-------------|--------|
| DBT-1 | `partition_by` config option wired through `stream_table.sql`, `create_stream_table.sql`, and `alter_stream_table.sql` | ~1d |
| DBT-2 | `fuse`, `fuse_ceiling`, `fuse_sensitivity` config options wired through the materialization and alter macro with change-detection logic | ~1–2d |
| DBT-3 | dbt docs update: README and SQL_REFERENCE.md dbt section | ~0.5d |

> **dbt macro updates subtotal: ~2–3.5 days**

### Multi-Tenant Scheduler Isolation (Phase 9)

> **In plain terms:** As deployments grow past 10 databases on a single cluster,
> all schedulers compete for the same global background-worker pool. One busy
> database can starve the others. Phase 9 gives operators per-database quotas
> and a priority queue so critical databases always get workers.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~C-3~~ | ~~**Per-database worker quotas.** Add `pg_trickle.per_database_worker_quota` GUC; priority ordering: IMMEDIATE > Hot > Warm > Cold STs; burst capacity up to 150% when other databases are under quota.~~ ✅ Done — GUC registered; `compute_per_db_quota()` with 80% burst; tier-aware `sort_ready_queue_by_priority`; 5 unit tests + 6 E2E tests | — | [src/scheduler.rs](src/scheduler.rs) |

> ⚠️ C-3 depends on C-1 (tiered scheduling) for Hot/Warm/Cold classification. If C-1
> is not ready, fall back to IMMEDIATE > all-other ordering with equal priority within
> each tier; add full tier-aware ordering as a follow-on when C-1 lands in v0.14.0.

> **Multi-tenant scheduler isolation subtotal: ~2–3 weeks**

### TPC-H Benchmark Harness (Phase 10)

> **In plain terms:** The existing TPC-H correctness suite (22/22 queries passing)
> has no timing infrastructure. Phase 10 adds benchmark mode so we can measure
> FULL vs DIFFERENTIAL speedups across all 22 queries — the only way to validate
> that A-2, D-4, and other v0.13.0 changes actually help on realistic analytical
> workloads, and to catch per-query regressions at larger scale factors.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TPCH-1 | **`TPCH_BENCH=1` benchmark mode for Phase 3.** Instrument `test_tpch_full_vs_differential` with warm-up cycles (`WARMUP_CYCLES=2`), reuse `extract_last_profile()` for `[PGS_PROFILE]` extraction, emit `[TPCH_BENCH]` structured output per cycle (`query=q01 tier=2 cycle=1 mode=DIFF ms=12.7 decision=0.41 merge=11.3 …`). Add `print_tpch_summary()` with per-query FULL/DIFF median, speedup, P95, and MERGE% table. | 4–5h | [PLAN_TPC_H_BENCHMARKING.md §3](plans/performance/PLAN_TPC_H_BENCHMARKING.md) | ✅ Done |
| TPCH-2 | **`just bench-tpch` / `bench-tpch-large` / `bench-tpch-fast` justfile targets.** `bench-tpch`: SF-0.01 with `TPCH_BENCH=1`; `bench-tpch-large`: SF-0.1 with 5 cycles; `bench-tpch-fast`: skip Docker image rebuild. Enables before/after measurement for every v0.13.0 optimization. | 15 min | [PLAN_TPC_H_BENCHMARKING.md §3](plans/performance/PLAN_TPC_H_BENCHMARKING.md) | ✅ Done |
| TPCH-3 | **TPC-H OpTree Criterion micro-benchmarks.** Add composite `OpTree` benchmarks to `benches/diff_operators.rs` representing TPC-H query shapes (`diff_tpch_q01`, `diff_tpch_q05`, `diff_tpch_q08`, `diff_tpch_q18`, `diff_tpch_q21`). Measures pure-Rust delta SQL generation time for complex multi-join/semi-join trees; catches DVM engine regressions without a running database. | 4h | [PLAN_TPC_H_BENCHMARKING.md §4](plans/performance/PLAN_TPC_H_BENCHMARKING.md) | ✅ Done |

> **TPC-H benchmark harness subtotal: ~1 day**

### SQL Coverage Cleanup (Phase 11)

> **In plain terms:** Three small SQL expression gaps that are unscheduled
> anywhere. Two are PG 16+ standard SQL syntax currently rejected with errors;
> one is an audit-gated correctness check for recursive CTEs with non-monotone
> operators. All are low-effort items that round out DVM coverage without
> adding scope risk.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| SQL-RECUR | **Recursive CTE non-monotone divergence audit.** Write an E2E test for a recursive CTE with `EXCEPT` or aggregation in the recursive term (`WITH RECURSIVE … SELECT … EXCEPT SELECT …`). If the test passes → downgrade G1.3 to P4 (verified correct, no code change). If it fails → add a guard in `diff_recursive_cte` that detects non-monotone recursive terms and rejects them with `ERROR: non-monotone recursive CTEs are not supported in DIFFERENTIAL mode — use FULL`. | 6–8h | [GAP_SQL_PHASE_7.md §G1.3](plans/sql/GAP_SQL_PHASE_7.md) |
| SQL-PG16-1 | **`IS JSON` predicate support (PG 16+).** `expr IS JSON`, `expr IS JSON OBJECT`, `expr IS JSON ARRAY`, `expr IS JSON SCALAR`, `expr IS JSON WITH UNIQUE KEYS` — standard SQL/JSON predicates rejected today. Add a `T_JsonIsPredicate` arm in `parser.rs`; the predicate is treated opaquely (no delta decomposition); it passes through to the delta SQL unchanged where the PG executor evaluates it natively. | 2–3h | [GAP_SQL_PHASE_6.md §G1.4](plans/sql/GAP_SQL_PHASE_6.md) |
| SQL-PG16-2 | **SQL/JSON constructor support (PG 16+).** `JSON_OBJECT(…)`, `JSON_ARRAY(…)`, `JSON_OBJECTAGG(…)`, `JSON_ARRAYAGG(…)` — standard SQL/JSON constructors (`T_JsonConstructorExpr`) currently rejected. Add opaque pass-through in `parser.rs`; treat as scalar expressions (no incremental maintenance of the JSON value itself); handle the aggregate variants the same way as other custom aggregates (full group rescan). | 4–6h | [GAP_SQL_PHASE_6.md §G1.5](plans/sql/GAP_SQL_PHASE_6.md) |

> **SQL coverage cleanup subtotal: ~1–2 days**

### DVM Engine Improvements (Phase 10)

> **In plain terms:** The delta SQL generated for deep multi-table joins
> (e.g., TPC-H Q05/Q09 with 6 joined tables) computes identical pre-change
> snapshots redundantly at every reference site, spilling multi-GB temporary
> files that exceed `temp_file_limit`. Nested semi-joins (Q20) exhibit an
> O(n²) blowup from fully materializing the right-side pre-change state.
> These improvements target the intermediate data volume directly in the
> delta SQL generator, with TPC-H 22/22 DIFFERENTIAL correctness as the
> measurable gate.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DI-1 | **Named CTE L₀ snapshots.** Emit per-leaf pre-change snapshots as named CTEs (`NOT MATERIALIZED` default; `MATERIALIZED` when reference count ≥ 3); deduplicate 3–10× redundant `EXCEPT ALL` evaluations per leaf. Targets Q05/Q09 temp spill root cause. | 2–3d | [PLAN_DVM_IMPROVEMENTS.md §DI-1](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-2 | **Pre-image read from change buffer + aggregate UPDATE-split.** Replace per-leaf `EXCEPT ALL` with a `NOT EXISTS` anti-join on `pk_hash` + direct `old_*` read. Per-leaf conditional fallback to `EXCEPT ALL` when delta exceeds `max_delta_fraction` for that leaf. Includes aggregate UPDATE-split: the 'D' side of `SUM(CASE WHEN …)` evaluates using `old_*` column values, superseding DI-8’s band-aid. | 3.5–5.5d | [PLAN_DVM_IMPROVEMENTS.md §DI-2](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-3 | **Group-key filtered aggregate old rescan.** Restrict non-algebraic aggregate `EXCEPT ALL` rescans to affected groups via `EXISTS (… IS NOT DISTINCT FROM …)` filter. NULL-safe. Independent quick win. | 0.5–1d | [PLAN_DVM_IMPROVEMENTS.md §DI-3](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-6 | **Lazy semi-join R_old materialization.** Skip `EXCEPT ALL` for unchanged semi-join right children; push down equi-join key as a filter when R_old is needed. Eliminates Q20-type O(n²) blowup. | 1–2d | [PLAN_DVM_IMPROVEMENTS.md §DI-6](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-4 | **Shared R₀ CTE cache.** Cache pre-change snapshot SQL by OpTree node identity to avoid regenerating duplicate inline subqueries for shared subtrees. Depends on DI-1. | 1–2d | [PLAN_DVM_IMPROVEMENTS.md §DI-4](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-5 | **Part 3 correction consolidation.** Consolidate per-node Part 3 correction CTEs for linear inner-join chains into a single term. | 2–3d | [PLAN_DVM_IMPROVEMENTS.md §DI-5](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-7 | **Scan-count-aware strategy selector.** `max_differential_joins` and `max_delta_fraction` per-stream-table options; auto-fallback to FULL refresh when join count or delta-rate threshold is exceeded. Complements DI-2's per-leaf fallback with a coarser per-ST guard at scheduler decision time. | 1–2d | [PLAN_DVM_IMPROVEMENTS.md §DI-7](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-8 | **SUM(CASE WHEN …) algebraic drift fix.** Detect `Expr::Raw("CASE …")` in `is_algebraically_invertible()` and fall back to GROUP_RESCAN. Q14 is unaffected (parsed as `ComplexExpression`, already GROUP_RESCAN). Correctness band-aid superseded by DI-2’s aggregate UPDATE-split. | ~0.5d | [PLAN_DVM_IMPROVEMENTS.md §DI-8](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-9 | **Scheduler skips IMMEDIATE-mode tables.** Raise `scheduler_interval_ms` GUC cap to 600,000 ms; return early from refresh-due check for `refresh_mode = IMMEDIATE` (verified safe: IMMEDIATE drains TABLE-source buffers synchronously; downstream CALCULATED tables detected via `has_stream_table_source_changes()` independently). | 0.5d | [PLAN_DVM_IMPROVEMENTS.md §DI-9](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-10 | **SF=1 benchmark validation gate.** Add `bench-tpch-sf1` justfile target (`TPCH_SF=1 TPCH_BENCH=1`). Gate v0.13.0 release on 22/22 queries at SF=1. CI: manual dispatch only (60–180 min runtime, 4h timeout). | ~0.5d | [PLAN_DVM_IMPROVEMENTS.md §DI-10](plans/performance/PLAN_DVM_IMPROVEMENTS.md) |
| DI-11 | **Predicate pushdown + deep-join L₀ threshold + planner hints.** (a) Enable `push_filter_into_cross_joins()` with scalar-subquery guard. (b) Deep-join L₀ threshold (4+ scans): skip L₀ reconstruction, use L₁ + Part 3 correction. (c) Deep-join planner hints (5+ scans): disable nestloop, raise work_mem, override temp_file_limit. Result: 22/22 TPC-H DIFFERENTIAL. | ~1d | — |

> **DI-2 promoted from v1.x:** CDC `old_*` column capture was completed as
> part of the typed-column CDC rewrite (already in production). DI-2 scope
> includes both the join-level pre-image capture (`NOT EXISTS` anti-join) and
> an aggregate UPDATE-split that uses `old_*` values for the 'D' side of
> SUM(CASE WHEN …), superseding DI-8's GROUP_RESCAN band-aid.

> **Implementation order:** DI-8 → DI-9 → DI-1 → DI-3 → DI-2 → DI-6 → DI-4 → DI-5 → DI-7 → DI-10 → DI-11

> **DVM improvements subtotal: ~2–3 weeks** (DI-8/DI-9 are small independent fixes; DI-1–DI-7 are the core engine work; DI-10 is a validation run; DI-11 is predicate pushdown + deep-join optimization)

### Regression-Free Testing Initiative (Q2 2026)

> Tracking: [TESTING_GAPS_2_IMPLEMENTATION_PROPOSAL.md](plans/testing/TESTING_GAPS_2_IMPLEMENTATION_PROPOSAL.md)

Addresses 9 structural weaknesses identified in the regression risk analysis.
Target: reduce regression escape rate from ~15% to <5%.

| Phase | Item | Status |
|-------|------|--------|
| P1 | Test infrastructure hardening: `#[must_use]` on poll helpers; `wait_for_condition` with exponential backoff; `assert_column_types_match` | ✅ Done (2026-03-28) |
| P2 | Join multi-cycle correctness: 7 tests — LEFT/RIGHT/FULL join, join-key update, both-sides DML, 4-table chain, NULL key | ✅ Done (2026-03-28) |
| P3 | Differential ≡ Full equivalence: 11 tests covering every major DVM operator class; `effective_refresh_mode` guard | ✅ Done (2026-03-28) |
| P4 | DVM operator execution: LATERAL MAX subquery multi-cycle (5 cycles) + recursive CTE org hierarchy multi-cycle (5 cycles) | ✅ Done (2026-03-28) |
| P5 | Failure recovery & schema evolution: 6 failure recovery tests (FR-1..6 in `e2e_failure_recovery_tests.rs`) + 5 schema evolution tests (SE-1..5 in `e2e_ddl_event_tests.rs`) | ✅ Done (2026-03-28) |
| P6 | MERGE template unit tests: 8 pure-Rust tests — `determine_refresh_action` (×5) + `build_is_distinct_clause` boundary (×3) in `src/refresh.rs` | ✅ Done (2026-03-28) |

> **v0.13.0 total: ~15–23 weeks** (Scalability: 6–8w, Partitioning: 5–8w, MERGE Profiling: 1–3w, dbt: 2–3.5d, Multi-tenant: 2–3w, TPC-H harness: ~1d, SQL cleanup: ~1–2d, DVM improvements: ~2–3w)

**Exit criteria:**
- [x] A-2: Columnar change tracking bitmask skips irrelevant rows; key column classification ✅, `__pgt_key_changed` annotation ✅, P5 value-only fast path ✅, `DiffResult.has_key_changed` signal propagation ✅, MERGE value-only UPDATE optimization ✅, upgrade script ✅ ✅ Done
- [x] D-4: Shared buffer serves multiple STs via per-source `changes_{oid}` naming; `pgt_change_tracking.tracked_by_pgt_ids` reference counting; `shared_buffer_stats()` observability; property-based test with 5–10 consumers (3 properties, 500 cases) ✅ Done; 5 E2E fan-out tests
- [x] PERF-2: `buffer_partitioning = 'auto'` activates RANGE(lsn) partitioned mode for high-throughput sources — throughput-based `should_promote_inner()` heuristic, `convert_buffer_to_partitioned()` runtime migration, 10 unit tests + 3 E2E tests, `docs/CONFIGURATION.md` updated ✅ Done
- [x] A1-1b: Multi-column RANGE partition keys work end-to-end; composite ROW() predicate triggers partition pruning; 3 E2E tests + 5 unit tests ✅ Done
- [x] A1-1c: `alter_stream_table(partition_by => …)` repartitions existing storage table without data loss; add/change/remove tested
- [x] A1-1d: LIST partitioning creates `PARTITION BY LIST` storage; IN-list predicate injected; single-column-only validated; 4 E2E tests pass
- [x] A1-3b: HASH partitioning uses per-partition MERGE loop; auto-creates N child partitions; `satisfies_hash_partition()` filter; 22 unit tests + 6 E2E tests ✅ Done
- [x] PART-WARN: `WARNING` emitted when default partition has rows after refresh; `warn_default_partition_growth()` on both FULL and DIFFERENTIAL paths ✅ Done
- [x] G14-MDED: Deduplication frequency profiling complete; `TOTAL_DIFF_REFRESHES` + `DEDUP_NEEDED_REFRESHES` shared-memory atomic counters; `pgtrickle.dedup_stats()` reports ratio; RFC threshold documented at ≥10% ✅ Done
- [x] PROF-DLT: `pgtrickle.explain_delta(st_name, format)` function captures delta query plans in text/json/xml/yaml; `PGS_PROFILE_DELTA=1` auto-capture to `/tmp/delta_plans/`; documented in SQL_REFERENCE.md ✅ Done
- [x] C-3: Per-database worker quota enforced; tier-aware priority sort (IMMEDIATE > Hot > Warm > Cold) implemented; GUC + E2E quota tests added; `compute_per_db_quota()` with burst at 80% cluster load ✅ Done
- [x] TPCH-1/2: `TPCH_BENCH=1` mode emits `[TPCH_BENCH]` lines + summary table; `just bench-tpch` and `bench-tpch-large` targets functional ✅ Done
- [x] TPCH-3: Five TPC-H OpTree Criterion benchmarks pass and run without a PostgreSQL backend ✅ Done
- [x] DBT-1/2/3: `partition_by`, `fuse`, `fuse_ceiling`, `fuse_sensitivity` exposed in dbt macros; change detection wired; integration tests added; README and SQL_REFERENCE.md updated ✅ Done
- [x] SQL-RECUR: Recursive CTE non-monotone audit complete; G1.3 downgraded to P4 — two Tier 3h E2E tests verify recomputation fallback is correct ✅ Done
- [x] SQL-PG16-1: `IS JSON` predicate accepted in DIFFERENTIAL defining queries; E2E tests in `e2e_expression_tests.rs` confirm correct delta behaviour ✅ Done
- [x] SQL-PG16-2: `JSON_OBJECT`, `JSON_ARRAY`, `JSON_OBJECTAGG`, `JSON_ARRAYAGG` accepted in DIFFERENTIAL defining queries; E2E tests in `e2e_expression_tests.rs` confirm correct delta behaviour ✅ Done
- [x] `scripts/check_upgrade_completeness.sh` passes (all catalog changes in `sql/pg_trickle--0.12.0--0.13.0.sql`) ✅ Done — 58 functions, 8 new columns, all covered
- [x] DI-8: `is_algebraically_invertible()` detects `Expr::Raw("CASE …")` and returns `false` for `SUM(CASE WHEN …)` (Q14 unaffected — `ComplexExpression`); Q12 removed from `DIFFERENTIAL_SKIP_ALLOWLIST`; 4 unit tests ✅ Done
- [x] DI-9: `scheduler_interval_ms` cap raised to 600,000 ms; scheduler skips IMMEDIATE-mode tables in `check_schedule()`; verified safe for CALCULATED dependants ✅ Done
- [x] DI-1: Named CTE L₀ snapshots implemented (`NOT MATERIALIZED` default, `MATERIALIZED` when ref ≥ 3); Q05/Q09 pass DIFFERENTIAL correctness ✅ Done
- [x] DI-2: `NOT EXISTS` anti-join replaces `EXCEPT ALL` in `build_pre_change_snapshot_sql()`; per-leaf conditional `EXCEPT ALL` fallback when delta > `max_delta_fraction`; aggregate UPDATE-split blocked on Q12 drift root cause (DI-8 band-aid retained) ✅ Done
- [x] DI-3: Already implemented — non-algebraic aggregate old rescan filtered via `EXISTS (… IS NOT DISTINCT FROM …)` to affected groups; NULL-safe ✅ Done
- [x] DI-6: Semi-join R_old lazy materialization with key push-down; Q20 DIFF passes at SF=0.01 ✅ Done
- [x] DI-4/5/7: R₀ cache (subset of DI-1), Part 3 threshold raised from 3→5, strategy selector + max_delta_fraction complete ✅ Done
- [x] DI-10: `bench-tpch-sf1` target added; 22/22 queries pass at SF=0.01 (3 cycles, zero drift) ✅ Done
- [x] DI-11: Predicate pushdown enabled with scalar-subquery guard; deep-join L₀ threshold (4 scans); deep-join planner hints (5+ total scans); 22/22 TPC-H DIFFERENTIAL ✅ Done
- [x] Extension upgrade path tested (`0.12.0 → 0.13.0`) ✅ Done

</details>

---


## v0.14.0 — Tiered Scheduling, UNLOGGED Buffers & Diagnostics

**Status: Released (2026-04-02).**

Tiered refresh scheduling, UNLOGGED change buffers, refresh mode diagnostics,
error-state circuit breaker, a full-featured TUI dashboard, security
hardening (SECURITY DEFINER triggers with explicit search_path), GHCR Docker
image, pre-deployment checklist, best-practice patterns guide, and
comprehensive E2E test coverage. See [CHANGELOG.md](CHANGELOG.md) for the
full feature list.

<details>
<summary>Completed items (click to expand)</summary>

### Quick Polish & Error State Circuit Breaker (Phase 1 + 1b) — ✅ Done

- **C4:** `pg_trickle.planner_aggressive` GUC consolidates `merge_planner_hints` + `merge_work_mem_mb`. Old GUCs deprecated.
- **DIAG-2:** Creation-time WARNING for group-rescan and low-cardinality algebraic aggregates. `agg_diff_cardinality_threshold` GUC added.
- **DOC-OPM:** Operator support matrix summary table linked from `SQL_REFERENCE.md`.
- **ERR-1:** Permanent failures immediately set `ERROR` status with `last_error_message`/`last_error_at`. API calls clear error state. E2E test pending.

### Manual Tiered Scheduling (Phase 2 — C-1) — ✅ Done

Tiered scheduling infrastructure was already in place since v0.11/v0.12 (`refresh_tier` column, `RefreshTier` enum, `ALTER ... SET (tier=...)`, scheduler multipliers). Phase 2 verified completeness and added:

- **C-1b:** NOTICE on tier demotion from Hot to Cold/Frozen, alerting operators to the effective interval change.
- **C-1c:** Scheduler tier-aware multipliers confirmed: Hot ×1, Warm ×2, Cold ×10, Frozen = skip. Gated by `pg_trickle.tiered_scheduling` (default `true` since v0.12.0).

### UNLOGGED Change Buffers (Phase 3 — D-1) — ✅ Done

- **D-1a:** `pg_trickle.unlogged_buffers` GUC (default `false`). New change buffer tables created as `UNLOGGED` when enabled, reducing WAL amplification by ~30%.
- **D-1b:** Crash recovery detection — scheduler detects UNLOGGED buffers emptied by crash (postmaster restart after last refresh) and auto-enqueues FULL refresh.
- **D-1c:** `pgtrickle.convert_buffers_to_unlogged()` utility function for converting existing logged buffers. Documents lock-window warning.
- **D-1e:** Documentation in `CONFIGURATION.md` and `SQL_REFERENCE.md`.

### Documentation: Best-Practice Patterns Guide (G16-PAT) — ✅ Done

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G16-PAT~~ | ~~**Best-practice patterns guide.** `docs/PATTERNS.md`: 6 patterns (Bronze/Silver/Gold, event sourcing, SCD type-1/2, high-fan-out, real-time dashboards, tiered refresh) with SQL examples, anti-patterns, and refresh mode recommendations.~~ | — | ✅ Done |

> **Patterns guide subtotal: ✅ Done**

### Long-Running Stability & Multi-Database Testing (G17-SOAK, G17-MDB) — ✅ Done

> Soak test validates zero worker crashes, zero ERROR states, and stable RSS
> under sustained mixed DML. Multi-database test validates catalog isolation,
> shared-memory independence, and concurrent correctness.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G17-SOAK~~ | ~~**Long-running stability soak test.** `tests/e2e_soak_tests.rs` with configurable duration, 5 source tables, mixed DML, health checks, RSS monitoring, correctness verification. `just test-soak` / `just test-soak-short`. CI job: schedule + manual dispatch.~~ | — | ✅ Done |
| ~~G17-MDB~~ | ~~**Multi-database scheduler isolation test.** `tests/e2e_mdb_tests.rs` with two databases, catalog isolation assertion, concurrent mutation cycles, correctness verification per database. `just test-mdb`. CI job: schedule + manual dispatch.~~ | — | ✅ Done |

> **Stability & multi-database testing subtotal: ✅ Done**

### Container Infrastructure (INFRA-GHCR)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| INFRA-GHCR | **GHCR Docker image.** `Dockerfile.ghcr` (pinned to `postgres:18.3-bookworm`) + `.github/workflows/ghcr.yml` workflow that builds a multi-arch (`linux/amd64` + `linux/arm64`) PostgreSQL 18.3 server image with pg_trickle pre-installed and all sensible GUC defaults baked in. Smoke-tests on amd64 before push. Published to `ghcr.io/grove/pg_trickle` on every `v*` tag with immutable (`<version>-pg18.3`), floating (`pg18`), and `latest` tags. Uses `GITHUB_TOKEN` — no extra secrets. | 4h | — | ✅ Done |

> **Container infrastructure subtotal: ✅ Done**

### Refresh Mode Diagnostics (DIAG-1) — ✅ Done

> Analyzes stream table workload characteristics and recommends the optimal
> refresh mode. Seven weighted signals (change ratio, empirical timing, query
> complexity, target size, index coverage, latency variance) produce a composite
> score with confidence level and human-readable explanation.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DIAG-1a~~ | ~~`src/diagnostics.rs` — pure signal-scoring functions + unit tests~~ | — | ✅ Done |
| ~~DIAG-1b~~ | ~~SPI data-gathering layer~~ | — | ✅ Done |
| ~~DIAG-1c~~ | ~~`pgtrickle.recommend_refresh_mode()` SQL function~~ | — | ✅ Done |
| ~~DIAG-1d~~ | ~~`pgtrickle.refresh_efficiency()` function~~ | — | ✅ Done |
| ~~DIAG-1e~~ | ~~E2E integration tests; upgrade migration~~ | — | ✅ Done |
| ~~DIAG-1f~~ | ~~Documentation: SQL_REFERENCE.md additions~~ | — | ✅ Done |

> The function synthesises 7 weighted signals (historical change ratio 0.30,
> empirical timing 0.35, current change ratio 0.25, query complexity 0.10,
> target size 0.10, index coverage 0.05, P95/P50 variance 0.05) into a
> composite score. Confidence degrades gracefully when history is sparse.

> **Diagnostics subtotal: ~3.5–7 days**

### Export Definition API (G15-EX) — ✅ Done

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G15-EX~~ | ~~**`export_definition(name TEXT)`** — export a stream table configuration as reproducible DDL~~ | — | ✅ Done |

> **G15-EX subtotal: ~1–2 days**

### TUI Tool (E3-TUI)

> **In plain terms:** A full-featured terminal user interface (TUI) for
> managing, monitoring, and diagnosing pg_trickle stream tables without
> touching SQL. Built with ratatui in Rust, it provides a real-time
> dashboard (think `htop` for stream tables), interactive dependency graph
> visualization, live refresh log, diagnostics with signal breakdown charts,
> CDC health monitoring, a GUC configuration editor, and a real-time alert
> feed — all navigable with keyboard shortcuts and a command palette.
> It also supports every original CLI command as one-shot subcommands for
> scripting and CI.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E3-TUI | TUI tool (`pgtrickle`) for interactive management and monitoring | 8–10d | [PLAN_TUI.md](plans/ui/PLAN_TUI.md) |

> **E3-TUI subtotal: ~8–10 days** (T1–T8 implemented: CLI skeleton with 18 subcommands, interactive dashboard with 15 views, watch mode with `--filter`, LISTEN/NOTIFY alerts with JSON parsing, async polling with force-poll, cascade staleness detection, DAG issue detection, sparklines, fuse detail panel, trigger inventory, context-sensitive help, docs/TUI.md)

### GUC Surface Consolidation (C4)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C4 | **Consolidate `merge_planner_hints` + `merge_work_mem_mb` into single `planner_aggressive` boolean.** Reduces GUC surface area; existing two GUCs become aliases that emit a deprecation notice. | ~1–2h | [PLAN_FEATURE_CLEANUP.md §C4](plans/PLAN_FEATURE_CLEANUP.md) |

> **C4 subtotal: ~1–2 hours**

### Documentation: Pre-Deployment Checklist (DOC-PDC) — ✅ Done

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~DOC-PDC~~ | ~~**Pre-deployment checklist page.** `docs/PRE_DEPLOYMENT.md`: 10-point checklist covering PG version, `shared_preload_libraries`, WAL configuration, PgBouncer compatibility, recommended GUCs, resource planning, monitoring, validation script. Cross-linked from GETTING_STARTED.md and INSTALL.md.~~ | — | ✅ Done |

> **DOC-PDC subtotal: ✅ Done**

### Documentation: Operator Mode Support Matrix Cross-Link (DOC-OPM)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DOC-OPM | **Cross-link operator support matrix from SQL_REFERENCE.md.** The 60+ operator × FULL/DIFFERENTIAL/IMMEDIATE matrix in DVM_OPERATORS.md is not discoverable from the page users actually read. Add a summary table and prominent link in SQL_REFERENCE.md §Supported SQL Constructs. | ~2–4h | [docs/DVM_OPERATORS.md](docs/DVM_OPERATORS.md) · [docs/SQL_REFERENCE.md](docs/SQL_REFERENCE.md) |

> **DOC-OPM subtotal: ~2–4 hours**

### Aggregate Mode Warning at Creation Time (DIAG-2)

> **In plain terms:** Queries with very few distinct GROUP BY groups (e.g. 5
> regions from 100K rows) are always faster with FULL refresh — differential
> overhead exceeds the cost of re-aggregating a tiny result set. Today users
> discover this only after benchmarking. A creation-time WARNING with an
> explicit recommendation prevents the surprise. The classification logic is
> already present in the DVM parser (aggregate strategy classification from
> `is_algebraically_invertible`, `is_group_rescan`); this item exposes it at
> the SQL boundary.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DIAG-2 | **Aggregate mode warning at `create_stream_table` time.** After parsing the defining query, inspect the top-level operator: if it is an `Aggregate` node containing non-algebraic (group-rescan) functions such as `MIN`, `MAX`, `STRING_AGG`, `ARRAY_AGG`, `BOOL_AND/OR`, emit a `WARNING` recommending `refresh_mode='full'` or `'auto'` and citing the group-rescan cost. For algebraic aggregates (`SUM`/`COUNT`/`AVG`), emit the warning only when the estimated group cardinality (from `pg_stats.n_distinct` on the GROUP BY columns) is below `pg_trickle.agg_diff_cardinality_threshold` (default: 1000 distinct groups), since below this threshold FULL is reliably faster. No behavior change — warning only. | ~2–4h | [plans/performance/REPORT_OVERALL_STATUS.md §12.3](plans/performance/REPORT_OVERALL_STATUS.md) |

> **DIAG-2 subtotal: ~2–4 hours**

### DIFFERENTIAL Refresh for Manual ST-on-ST Path (FIX-STST-DIFF)

> **Background:** When a stream table reads from another stream table
> (`calculated` schedule), the scheduler propagates changes via a per-ST
> change buffer (`pgtrickle_changes.changes_pgt_{id}`) and performs a true
> DIFFERENTIAL DVM refresh against that buffer. The manual
> `pgtrickle.refresh_stream_table()` path does not: it currently falls back
> to an unconditional `TRUNCATE + INSERT` (FULL refresh) for every call.
>
> This was introduced as a correctness fix in v0.13.0 (PR #371) to close a
> scheduler race where the previous no-op guard could leave stale data in
> place. The FULL fallback is correct but inefficient — it pays a full table
> scan of all upstream STs even when only a small delta is present.
>
> **What needs to happen:** Wire `execute_manual_differential_refresh` to
> use the same `changes_pgt_` change buffers the scheduler already writes.
> When a manual refresh is requested for a `calculated` ST that has a stored
> frontier, check each upstream ST's change buffer for rows with
> `lsn > frontier.get_st_lsn(upstream_pgt_id)`. If new rows exist, apply
> the DVM delta SQL (same as `execute_differential_refresh`). If no rows
> exist beyond the frontier, return a true no-op. This also fixes the
> pre-existing `test_st_on_st_uses_differential_not_full` E2E failure.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~FIX-STST-DIFF~~ | ~~**DIFFERENTIAL manual refresh for ST-on-ST.** In `execute_manual_differential_refresh` (`src/api.rs`), replace the unconditional FULL fallback for `has_st_source` with a proper change-buffer delta path: read rows from `changes_pgt_{upstream_pgt_id}` beyond the stored frontier LSN, run DVM differential SQL, advance the frontier. Matches the scheduler path exactly. Fixes `test_st_on_st_uses_differential_not_full`.~~ | — | ✅ Done |

> **FIX-STST-DIFF subtotal: ~1–2 days**

> **v0.14.0 total: ~2–6 weeks + ~1wk patterns guide + ~2–4 days stability tests + ~3.5–7 days diagnostics + ~1–2d export API + ~8–10d TUI + ~0.5d docs + ~2–4h aggregate warning + ~1–2d ST-on-ST diff manual path**

**Exit criteria:**
- [x] C-1: Tier classification with manual assignment; Cold STs skip refresh correctly; E2E tested ✅ Done
- [x] D-1: UNLOGGED change buffers opt-in (`unlogged_buffers = false` by default); crash-recovery FULL-refresh path tested; E2E tested ✅ Done
- [x] G16-PAT: Patterns guide published in `docs/PATTERNS.md` covering 6 patterns ✅ Done
- [x] G17-SOAK: Soak test passes with zero worker crashes, zero zombie stream tables, stable memory ✅ Done
- [x] G17-MDB: Multi-database scheduler isolation verified ✅ Done
- [x] DIAG-1: `recommend_refresh_mode()` + `refresh_efficiency()` implemented with 7 signals; E2E tested; tutorial published ✅ Done
- [x] DIAG-2: WARNING emitted at creation time for group-rescan and low-cardinality aggregates; threshold configurable ✅ Done
- [x] G15-EX: `export_definition(name TEXT)` returns valid reproducible DDL; round-trip tested ✅ Done
- [x] E3-TUI: `pgtrickle` TUI binary builds as workspace member; one-shot CLI commands functional with `--format json`; interactive dashboard launches with no subcommand; 15 views with cascade staleness, issue detection, sparklines, force-poll, NOTIFY, and context-sensitive help; documented in `docs/TUI.md` ✅ Done
- [x] C4: `merge_planner_hints` and `merge_work_mem_mb` consolidated into `planner_aggressive` ✅ Done
- [x] DOC-PDC: Pre-deployment checklist published in `docs/PRE_DEPLOYMENT.md` ✅ Done
- [x] DOC-OPM: Operator mode support matrix summary and link added to SQL_REFERENCE.md ✅ Done
- [x] FIX-STST-DIFF: Manual DIFFERENTIAL refresh for ST-on-ST path ✅ Done
- [x] INFRA-GHCR: `ghcr.io/grove/pg_trickle` multi-arch image builds, smoke-tests, and pushes on `v*` tags ✅ Done
- [x] ERR-1: Error-state circuit breaker with E2E test coverage ✅ Done
- [x] Extension upgrade path tested (`0.13.0 → 0.14.0`) ✅ Done

</details>

---


## v0.15.0 — External Test Suites & Integration

**Status: Released (2026-04-03).** All 20 roadmap items complete.

**Goal:** Validate correctness against independent query corpora and ship the
dbt integration as a formal release.

<details>
<summary>Completed items (click to expand)</summary>

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
> exit criterion for 0.11.0. TS3 (Nexmark) remains in 0.15.0. If TS1/TS2 slip
> from 0.11.0, they land here.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~TS1~~ | ~~sqllogictest: run PostgreSQL sqllogic suite through pg_trickle DIFFERENTIAL mode~~ ➡️ Pulled to v0.11.0 | 2–3d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| ~~TS2~~ | ~~JOB (Join Order Benchmark): correctness baseline and refresh latency profiling~~ ➡️ Pulled to v0.11.0 | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS3 | Nexmark streaming benchmark: sustained high-frequency DML correctness | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |

> **External test suites subtotal: ~1–2 days (TS3 only; TS1/TS2 in v0.11.0)** -- ✅ TS3 complete

### Documentation Review

> **In plain terms:** A full documentation review polishes everything so the
> product is ready to be announced to the wider PostgreSQL community.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| I2 | Complete documentation review & polish | 4--6h | [docs/](docs/) |

> **Documentation subtotal: ✅ Done**

### Bulk Create API (G15-BC)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G15-BC | ~~**`bulk_create(definitions JSONB)`** — create multiple stream tables and their CDC triggers in a single transaction. Useful for dbt/CI pipelines that manage many STs programmatically.~~ ✅ Done | ~2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |

> **G15-BC subtotal: ✅ Completed**

### Parser Modularization (G13-PRF) -- ✅ Done

> **In plain terms:** At ~21,000 lines, `parser.rs` was too large to maintain
> safely. Split into 5 sub-modules by concern -- zero behavior change.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G13-PRF | ~~**Modularize `src/dvm/parser.rs`.**~~ ✅ Done. Split into `mod.rs`, `types.rs`, `validation.rs`, `rewrites.rs`, `sublinks.rs`. Added `// SAFETY:` comments to all ~750 `unsafe` blocks (~676 newly documented). | ~3–4wk | [plans/performance/REPORT_OVERALL_STATUS.md §13](plans/performance/REPORT_OVERALL_STATUS.md) |

> **G13-PRF subtotal: ✅ Completed**

### Watermark Hold-Back Mode (WM-7) -- ✅ Done

> **In plain terms:** The watermark gating system (shipped in v0.7.0) lets
> ETL producers signal their progress. Hold-back mode adds stuck detection:
> when a watermark is not advanced within a configurable timeout, downstream
> stream tables are paused and operators are notified.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| WM-7 | **Watermark hold-back mode.** `watermark_holdback_timeout` GUC detects stuck watermarks; pauses downstream gated STs; emits `pgtrickle_alert` NOTIFY with `watermark_stuck` event; auto-resumes with `watermark_resumed` event when watermark advances. | ✅ Done | [PLAN_WATERMARK_GATING.md §4.1](plans/sql/PLAN_WATERMARK_GATING.md) |

> **WM-7 subtotal: ✅ Done**

### Delta Cost Estimation (PH-E1) — ✅ Done

> **In plain terms:** Before executing the MERGE, runs a capped COUNT on the
> delta subquery to estimate output cardinality. If the count exceeds
> `pg_trickle.max_delta_estimate_rows`, emits a NOTICE and falls back to FULL
> refresh to prevent OOM or excessive temp-file spills.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PH-E1 | **Delta cost estimation.** Capped `SELECT count(*) FROM (delta LIMIT N+1)` before MERGE execution. `max_delta_estimate_rows` GUC (default: 0 = disabled). Falls back to FULL + NOTICE when exceeded. | — | [PLAN_PERFORMANCE_PART_9.md §Phase E](plans/performance/PLAN_PERFORMANCE_PART_9.md) |

> **PH-E1 subtotal: ✅ Complete**

### dbt Hub Publication (I3) — ✅ Done

> **In plain terms:** `dbt-pgtrickle` is now prepared for dbt Hub publication.
> The `dbt_project.yml` is version-synced (0.15.0), README documents both
> git and Hub install methods, and a submission guide documents the hubcap
> PR process. Actual Hub listing requires creating a standalone `grove/dbt-pgtrickle`
> repository and submitting a PR to `dbt-labs/hubcap`.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| I3 | Prepared `dbt-pgtrickle` for [dbt Hub](https://hub.getdbt.com/) publication. Version synced to 0.15.0, README updated with Hub install snippet, submission guide written. Hub listing pending separate repo creation + hubcap PR. | 2–4h | [dbt-pgtrickle/](dbt-pgtrickle/) · [docs/integrations/dbt-hub-submission.md](docs/integrations/dbt-hub-submission.md) |

> **I3 subtotal: ~2–4 hours** — ✅ Complete

### Hash-Join Planner Hints (PH-D2) — ✅ Done

> **In plain terms:** Added `pg_trickle.merge_join_strategy` GUC that lets
> operators manually override the join strategy used during MERGE. Values:
> `auto` (default heuristic), `hash_join`, `nested_loop`, `merge_join`.
> The existing delta-size heuristics remain the default (`auto`).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PH-D2 | **Hash-join planner hints.** Added `merge_join_strategy` GUC with manual override for join strategy during MERGE. `auto` preserves existing delta-size heuristics; `hash_join`/`nested_loop`/`merge_join` force specific strategies. | 3–5d | [PLAN_PERFORMANCE_PART_9.md §Phase D](plans/performance/PLAN_PERFORMANCE_PART_9.md) |

> **PH-D2 subtotal: ~3–5 days** — ✅ Complete

### Shared-Memory Template Cache Research Spike (G14-SHC-SPIKE)

> **In plain terms:** Every new database connection that triggers a refresh
> pays a 15–50ms cold-start cost to regenerate the MERGE SQL template. With
> PgBouncer in transaction mode, this happens on every refresh cycle. This
> milestone scopes a research spike only: write an RFC, build a prototype,
> measure whether DSM-based caching eliminates the cold-start. Full
> implementation stays in v0.16.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G14-SHC-SPIKE | **Shared-memory template cache research spike.** Write an RFC for DSM + lwlock-based MERGE SQL template caching. Build a prototype benchmark to validate cold-start elimination. Full implementation deferred to v0.16.0. | 2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |

> **G14-SHC-SPIKE subtotal: ~2–3 days** -- ✅ RFC complete (plans/performance/RFC_SHARED_TEMPLATE_CACHE.md)

### TRUNCATE Capture for Trigger-Mode CDC (TRUNC-1)

> **In plain terms:** WAL-mode CDC detects TRUNCATE on source tables and
> marks downstream stream tables for reinitialization. But trigger-mode CDC
> has no TRUNCATE handler — a `TRUNCATE` silently leaves the stream table
> stale. Adding a DDL event trigger that catches TRUNCATE and flags affected
> STs closes this correctness gap.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TRUNC-1 | ~~**TRUNCATE capture for trigger-mode CDC.** Add a DDL event trigger or statement-level trigger that detects TRUNCATE on source tables in trigger CDC mode and marks downstream STs for `needs_reinit`.~~ ✅ Done — CDC TRUNCATE triggers write `action='T'` marker; refresh engine detects and falls back to FULL. | 4–6h | [plans/adrs/PLAN_ADRS.md](plans/adrs/PLAN_ADRS.md) ADR-070 |

> **TRUNC-1 subtotal: ✅ Completed**

### Volatile Function Policy GUC (VOL-1)

> **In plain terms:** Volatile functions (`random()`, `clock_timestamp()`,
> etc.) are correctly rejected at stream table creation time in DIFFERENTIAL
> and IMMEDIATE modes. But there’s no way for users to override this — some
> want volatile functions in FULL mode. Adding a `volatile_function_policy`
> GUC with `reject`/`warn`/`allow` modes gives operators control.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| VOL-1 | ~~**`pg_trickle.volatile_function_policy` GUC.** Add a GUC with values `reject` (default), `warn`, `allow` to control volatile function handling. `reject` preserves current behavior; `warn` emits WARNING but allows creation; `allow` silently permits (user accepts correctness risk).~~ ✅ Done | 3–5h | [plans/sql/PLAN_NON_DETERMINISM.md](plans/sql/PLAN_NON_DETERMINISM.md) |

> **VOL-1 subtotal: ✅ Completed**

### Spill-Aware Refresh (PH-E2)

> **In plain terms:** After PH-E1 adds pre-flight cost estimation, PH-E2
> adds post-flight monitoring: track `temp_bytes` from `pg_stat_statements`
> after each refresh cycle and auto-adjust if spill is excessive.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PH-E2 | ~~**Spill-aware refresh.** Monitor `temp_bytes` from `pg_stat_statements` after each refresh cycle. If spill exceeds threshold 3 consecutive times, automatically increase `per-ST work_mem` override or switch to FULL. Expose in `explain_st()` as `spill_history`.~~ ✅ Done | 1–2 wk | [PLAN_PERFORMANCE_PART_9.md §Phase E](plans/performance/PLAN_PERFORMANCE_PART_9.md) |

> **PH-E2 subtotal: ✅ Completed**

### ORM Integration Guides (E5)

> **In plain terms:** Documentation showing how popular ORMs (SQLAlchemy,
> Django, etc.) interact with stream tables — model definitions, migrations,
> and freshness checks. Documentation-only work.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E5 | ORM integrations guide (SQLAlchemy, Django, etc.) | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

> **E5 subtotal: ✅ Done**

### Flyway / Liquibase Migration Support (E4)

> **In plain terms:** Documentation showing how standard migration frameworks
> interact with stream tables — CREATE/ALTER/DROP patterns, handling CDC
> triggers across schema migrations. Documentation-only work.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E4 | Flyway / Liquibase migration support | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

> **E4 subtotal: ✅ Done**

### JOIN Key Change + DELETE Correctness Fix (EC-01) — ✅ Done (pre-existing)

> **In plain terms:** The phantom-row-after-DELETE bug was fixed in v0.14.0
> via the R₀ pre-change snapshot strategy. Part 1 of the JOIN delta is split
> into 1a (inserts ⋈ R₁) + 1b (deletes ⋈ R₀), ensuring DELETE deltas always
> find the old join partner. The fix was extended to all join depths via the
> EC-01B-1 per-leaf CTE strategy, and regression tests (EC-01B-2) cover
> TPC-H Q07, Q08, Q09.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC-01 | **R₀ pre-change snapshot for JOIN key change + DELETE.** Part 1 split into 1a (inserts ⋈ R₁) + 1b (deletes ⋈ R₀). Applied to INNER/LEFT/FULL JOIN. Closes G1.1. | — | [GAP_SQL_PHASE_7.md §G1.1](plans/sql/GAP_SQL_PHASE_7.md) |

> **EC-01 subtotal: ✅ Complete (implemented in v0.14.0)**

### Multi-Level ST-on-ST Testing (STST-3)

> **In plain terms:** FIX-STST-DIFF (v0.14.0) fixed 2-level
> stream-table-on-stream-table DIFFERENTIAL refresh. Some 3-level cascade
> tests exist, but systematic coverage for 3+ level chains — including
> mixed refresh modes, concurrent DML at multiple levels, and DELETE/UPDATE
> propagation through deep chains — is missing. This adds a dedicated test
> matrix to prevent regressions as cascade depth increases.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| STST-3 | **Multi-level ST-on-ST test matrix (3+ levels).** Systematic coverage: 3-level and 4-level chains, INSERT/UPDATE/DELETE propagation, mixed DIFFERENTIAL/FULL modes, concurrent DML at multiple levels, correctness comparison against materialized-view baseline. | 3–5d | [e2e_cascade_regression_tests.rs](tests/e2e_cascade_regression_tests.rs) |

> **STST-3 subtotal: ✅ Done**

### Circular Dependencies + IMMEDIATE Mode (CIRC-IMM)

> **In plain terms:** Circular dependencies are rejected at creation time
> (EC-30), but the interaction between near-circular topologies (e.g.
> diamond dependencies with IMMEDIATE triggers on both sides) and IMMEDIATE
> mode is untested territory. This adds targeted testing and, if needed,
> hardening to ensure IMMEDIATE mode doesn't deadlock or produce incorrect
> results on complex dependency graphs. **Conditional P1 — can slip to
> v0.16.0 if no issues surface during other IMMEDIATE-mode work.**

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| CIRC-IMM | **Circular-dependency + IMMEDIATE mode hardening.** Test: diamond deps with IMMEDIATE triggers, near-circular topologies, lock ordering under concurrent DML. Add deadlock detection / timeout guard if issues found. | 3–5d | [PLAN_EDGE_CASES.md §EC-30](plans/PLAN_EDGE_CASES.md) · [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) |

> **CIRC-IMM subtotal: ✅ Done**

### Cross-Session MERGE Cache Staleness Fix (G8.1)

> **In plain terms:** When session A alters a stream table's defining query,
> session B's cached MERGE SQL template remains stale until B encounters a
> refresh error or reconnects. Adding a catalog version counter that is
> bumped on every ALTER QUERY and checked before each refresh closes this
> race window.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G8.1 | ~~**Cross-session MERGE cache invalidation.** Add a `catalog_version` counter to `pgt_stream_tables`, bump on ALTER QUERY / DROP / reinit. Before each refresh, compare cached version to catalog; regenerate template on mismatch.~~ ✅ Done — existing `CACHE_GENERATION` counter + `defining_query_hash` provides cross-session + per-ST invalidation without a schema change. | 4–6h | — |

> **G8.1 subtotal: ✅ Completed**

### `explain_st()` Enhancements (EXPL-ENH) — ✅ Done

> **In plain terms:** Small quality-of-life improvements to the diagnostic
> function: refresh timing statistics, partition source info, and a dependency-graph
> visualization snippet in DOT format.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EXPL-ENH | **`explain_st()` enhancements.** Added: (a) refresh timing stats (min/max/avg/latest duration from last 20 refreshes), (b) source partition info for partitioned tables, (c) dependency sub-graph visualization in DOT format. | 4–8h | [PLAN_FEATURE_CLEANUP.md](plans/PLAN_FEATURE_CLEANUP.md) |

> **EXPL-ENH subtotal: ~4–8 hours** — ✅ Complete

### CNPG Operator Hardening (R4)

> **In plain terms:** Kubernetes-native improvements for the CloudNativePG
> integration: adopt K8s 1.33+ native ImageVolume (replacing the init-container
> workaround), add liveness/readiness probe integration for pg_trickle health,
> and test failover behavior with stream tables.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| R4 | **CNPG operator hardening.** Adopt K8s 1.33+ native ImageVolume, add pg_trickle health to CNPG liveness/readiness probes, test primary→replica failover with active stream tables. | 4–6h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |

> **R4 subtotal: ~4–6 hours** -- ✅ Complete

> **v0.15.0 total: ~52–90h + ~2–3d bulk create + ~3–5d planner hints + ~2–3d cache spike + ~3–4wk parser + ~1–2wk watermark + ~2–4wk delta cost/spill + ~2–3d EC-01 + ~3–5d ST-on-ST + ~3–5d CIRC-IMM**

**Exit criteria:**
- [x] At least one external test corpus (sqllogictest, JOB, or Nexmark) passes
- [x] Complete documentation review done
- [x] G15-BC: `pgtrickle.bulk_create(definitions JSONB)` creates all STs and CDC triggers atomically; tested with 10+ definitions in a single call
- [x] G13-PRF: `parser.rs` split into 5 sub-modules; zero behavior change; all existing tests pass
- [x] WM-7: Stuck watermarks detected and downstream STs paused; `watermark_stuck` alert emitted; auto-resume on watermark advance
- [x] PH-E1: Delta cost estimation via capped COUNT on delta subquery; `max_delta_estimate_rows` GUC; FULL downgrade + NOTICE when threshold exceeded
- [x] PH-E2: Spill-aware auto-adjustment triggers after 3 consecutive spills; `spill_info` exposed in `explain_st()`
- [x] PH-D2: `merge_join_strategy` GUC with manual override (`auto`/`hash_join`/`nested_loop`/`merge_join`)
- [x] G14-SHC-SPIKE: RFC written; prototype benchmark validates or invalidates DSM-based approach
- [x] I2: Complete documentation review done -- CONFIGURATION.md GUCs documented (40+), SQL_REFERENCE.md gaps filled, FAQ refs fixed
- [x] TRUNC-1: TRUNCATE on trigger-mode CDC source marks downstream STs for reinit; tested end-to-end
- [x] VOL-1: `volatile_function_policy` GUC controls volatile function handling; `reject`/`warn`/`allow` modes tested
- [x] I3: `dbt-pgtrickle` prepared for dbt Hub; submission guide written; Hub listing pending separate repo + hubcap PR
- [x] E4: Flyway / Liquibase integration guide published in `docs/integrations/flyway-liquibase.md`
- [x] E5: ORM integration guides (SQLAlchemy, Django) published in `docs/integrations/orm.md`
- [x] EC-01: R₀ pre-change snapshot ensures DELETE deltas find old join partners; unit + TPC-H regression tests confirm correctness
- [x] STST-3: 3-level and 4-level ST-on-ST chains tested with INSERT/UPDATE/DELETE propagation; mixed modes covered
- [x] CIRC-IMM: Diamond + near-circular IMMEDIATE topologies tested; no deadlocks or incorrect results
- [x] G8.1: Cross-session MERGE cache invalidation via catalog version counter; tested with concurrent ALTER QUERY + refresh
- [x] EXPL-ENH: `explain_st()` shows refresh timing stats, source partition info, and dependency sub-graph (DOT format)
- [x] R4: CNPG operator hardening — ImageVolume, health probes, failover tested
- [x] G13-PRF: `parser.rs` split into 5 sub-modules; all ~750 `unsafe` blocks have `// SAFETY:` comments; zero behavior change; all existing tests pass
- [x] Extension upgrade path tested (`0.14.0 → 0.15.0`)
- [x] `just check-version-sync` passes

</details>

---


## v0.16.0 — Performance & Refresh Optimization

**Status: Released (2026-04-06).**

Faster refreshes across the board: sub-1% deltas use DELETE+INSERT instead of
MERGE, insert-only stream tables auto-detect and skip the MERGE join, algebraic
aggregates apply pinpoint updates, and a cross-backend template cache eliminates
cold-start latency. Automated benchmark regression gating prevents future
performance degradation.

<details>
<summary>Completed items (click to expand)</summary>

**Goal:** Attack the MERGE bottleneck from multiple angles — alternative merge
strategies, algebraic aggregate shortcuts, append-only bypass, delta filtering,
change buffer compaction, shared-memory template caching — close critical test
coverage gaps to validate these new paths.

### MERGE Alternatives & Planner Control (Phase D)

> **In plain terms:** MERGE dominates 70–97% of refresh time. This explores
> whether replacing MERGE with DELETE+INSERT (or INSERT ON CONFLICT + DELETE)
> is faster for specific patterns — particularly for small deltas against
> large stream tables where the MERGE join is the bottleneck.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~PH-D1~~ | ~~**DELETE+INSERT strategy.** For stream tables where delta is <1% of target, replace MERGE with `DELETE WHERE __pgt_row_id IN (delta_deletes)` + `INSERT ... SELECT FROM delta_inserts`. Benchmark against MERGE for 1K/10K/100K deltas against 1M/10M targets. Gate behind `pg_trickle.merge_strategy = 'auto'\|'merge'\|'delete_insert'` GUC.~~ | ~~1–2 wk~~ | ~~[PLAN_PERFORMANCE_PART_9.md §Phase D](plans/performance/PLAN_PERFORMANCE_PART_9.md)~~ |

> **MERGE alternatives subtotal: ~1–2 weeks**

### Algebraic Aggregate UPDATE Fast-Path (B-1)

> **In plain terms:** The current aggregate delta rule recomputes entire
> groups where the GROUP BY key appears in the delta. For a group with 100K
> rows where 1 row changed, the aggregate re-scans all 100K rows in that
> group. For decomposable aggregates (`SUM`/`COUNT`/`AVG`), a direct
> `UPDATE target SET col = col + Δ` replaces the full MERGE join — dropping
> aggregate refresh from O(group_size) to O(1) per group.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| B-1 | **Algebraic aggregate UPDATE fast-path.** For `GROUP BY` queries where all aggregates are algebraically invertible (`SUM`/`COUNT`/`AVG`), replace the MERGE with a direct `UPDATE target SET col = col + Δ WHERE group_key = ?` for existing groups, plus `INSERT` for newly-appearing groups and `DELETE` for groups whose count reaches zero. Eliminates the MERGE join overhead — the dominant cost for aggregate refresh when group cardinality is high. Requires adding `__pgt_aux_count` / `__pgt_aux_sum` auxiliary columns to the stream table. Fallback to existing MERGE path for non-algebraic aggregates (`MIN`, `MAX`, `STRING_AGG`, etc.). Gate behind `pg_trickle.aggregate_fast_path` GUC (default `true`). Expected impact: **5–20× apply-time reduction** for high-cardinality GROUP BY (10K+ distinct groups); aggregate scenarios at 100K/1% projected to drop from ~50ms to sub-1ms apply time. | 4–6 wk | [plans/performance/PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) · [plans/sql/PLAN_TRANSACTIONAL_IVM.md §Phase 4](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |

> **B-1 subtotal: ~4–6 weeks**

### Append-Only Stream Tables — MERGE Bypass (A-3-AO)

> **In plain terms:** When a stream table's sources are insert-only (e.g.
> event logs, append-only tables where CDC never sees DELETE/UPDATE), the
> MERGE is pure overhead — every delta row is an INSERT, never a match.
> Bypassing MERGE entirely with a plain `INSERT INTO st SELECT ... FROM delta`
> removes the join against the target table, takes only `RowExclusiveLock`,
> and is the single highest-payoff optimization for event-sourced architectures.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~A-3-AO~~ | ~~**Append-only stream table fast path.** Expose an explicit `CREATE STREAM TABLE … APPEND ONLY` declaration. When set, refresh uses `INSERT INTO st SELECT ... FROM delta` instead of MERGE — no target-table join, `RowExclusiveLock` only. CDC-observed heuristic fallback: if no DELETE/UPDATE has been seen, use the fast path; fall back to MERGE on first non-insert. Benchmark against MERGE for 1K/10K/100K append deltas.~~ | ~~1–2 wk~~ | ~~[plans/performance/PLAN_NEW_STUFF.md §A-3](plans/performance/PLAN_NEW_STUFF.md)~~ |

> **A-3-AO subtotal: ~1–2 weeks**

### Delta Predicate Pushdown (B-2)

> **In plain terms:** For a query like `SELECT ... FROM orders WHERE status =
> 'shipped'`, if a CDC change row has `status = 'pending'`, the delta
> processes it through scan → filter → discard. All the scan and join work
> is wasted. Pushing the WHERE predicate down into the change buffer scan
> eliminates irrelevant rows before any join processing begins — a 5–10×
> reduction in delta row volume for selective queries.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~B-2~~ | ~~**Delta predicate pushdown.** During OpTree construction, identify `Filter` nodes whose predicates reference only columns from a single source table. Inject these predicates into the `delta_scan` CTE as additional WHERE clauses (including `OR old_col = 'value'` for DELETE correctness). Expected impact: **5–10× delta row reduction** for queries with < 10% selectivity.~~ | ~~2–3 wk~~ | ~~[plans/performance/PLAN_NEW_STUFF.md §B-2](plans/performance/PLAN_NEW_STUFF.md)~~ |

> **B-2 subtotal: ~2–3 weeks**

### Shared-Memory Template Caching (G14-SHC)

> **In plain terms:** Every new database connection that triggers a refresh
> pays a 15–50ms cold-start cost to regenerate the MERGE SQL template. With
> PgBouncer in transaction mode, this happens on every single refresh cycle.
> Shared-memory caching stores compiled templates in PostgreSQL DSM so they
> survive across connections — eliminating the cold-start entirely for
> steady-state workloads.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G14-SHC | **Shared-memory template caching (implementation).** Full implementation of DSM + lwlock-based MERGE SQL template caching, building on the G14-SHC-SPIKE RFC from v0.15.0. | ~2–3wk | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |

> **G14-SHC subtotal: ~2–3 weeks**

### ~~PostgreSQL 19 Forward-Compatibility (A3)~~ — Moved to v1.0.0

> PG 19 beta not available in time. Items A3-1 through A3-4 deferred
> to v1.0.0 milestone.

### Change Buffer Compaction (C-4)

> **In plain terms:** A high-churn source table can accumulate thousands of
> changes to the same row between refresh cycles — an INSERT followed by 10
> UPDATEs followed by a DELETE is really just "nothing happened." Compaction
> merges multiple changes to the same row ID into a single net change before
> the delta query runs, reducing change buffer size by 50–90% for high-churn
> tables. This directly reduces work for every downstream path (MERGE,
> DELETE+INSERT, append-only INSERT, predicate pushdown).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~C-4~~ | ~~**Change buffer compaction.** Before delta-query execution, merge multiple changes to the same `__pgt_row_id` into a single net change: INSERT+DELETE cancel out; consecutive UPDATEs collapse to one. Trigger on buffer exceeding `pg_trickle.compact_threshold` rows (default: 100K). Expected impact: **50–90% reduction in change buffer size** for high-churn tables.~~ | ~~2–3 wk~~ | ~~[plans/performance/PLAN_NEW_STUFF.md §C-4](plans/performance/PLAN_NEW_STUFF.md)~~ |

> **C-4 subtotal: ~2–3 weeks**

### Test Coverage Hardening (TG2)

> **In plain terms:** The performance optimizations in this release change
> core refresh paths (MERGE alternatives, aggregate fast-path, append-only
> bypass, predicate pushdown). Before and alongside these changes, critical
> test coverage gaps need closing — particularly around operators and
> scenarios where bugs could hide silently. These gaps were identified in
> the TESTING_GAPS_2 audit.

#### High-Priority Gaps

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~TG2-WIN~~ | ~~**Window function DVM execution tests.** ~5 unit tests exist but 0 DVM execution tests. Add execution-level tests for ROW_NUMBER, RANK, DENSE_RANK, LAG/LEAD delta behavior across INSERT/UPDATE/DELETE cycles.~~ | ~~3–5d~~ | ~~[TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md)~~ |
| ~~TG2-JOIN~~ | ~~**Join multi-cycle UPDATE/DELETE correctness.** E2E join tests are INSERT-only; no UPDATE/DELETE differential cycles. Add systematic multi-cycle coverage for INNER/LEFT/FULL JOIN with UPDATE and DELETE propagation. Risk: silent data corruption in production workloads.~~ | ~~3–5d~~ | ~~[TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md)~~ |
| ~~TG2-EQUIV~~ | ~~**Differential ≡ Full equivalence validation.** Only CTEs validated; joins and aggregates lack equivalence proof. Add a test harness that runs every defining query in both DIFFERENTIAL and FULL mode and asserts identical results. Critical for trusting the new optimization paths.~~ | ~~3–5d~~ | ~~[TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md)~~ |

#### Medium-Priority Gaps

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TG2-MERGE | **refresh.rs MERGE template unit tests.** Only helpers/enums tested; the core MERGE SQL template generation is untested at the unit level. | 2–3d | [TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md) |
| TG2-CANCEL | **Timeout/cancellation during refresh.** Zero tests for `statement_timeout`, `pg_cancel_backend()` during active refresh. Risk: silent failures or resource leaks under production load. | 1–2d | [TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md) |
| TG2-SCHEMA | **Source table schema evolution.** Partial DDL tests exist; type changes and column renames are thin. Risk: silent data corruption on schema change. | 2–3d | [TESTING_GAPS_2.md](plans/testing/TESTING_GAPS_2.md) |

> **TG2 subtotal: ~2–4 weeks (high-priority) + ~1–2 weeks (medium-priority)**

### Performance Regression CI (BENCH-CI)

> **In plain terms:** v0.16.0 changes core refresh paths (MERGE alternatives,
> aggregate fast-path, append-only bypass, predicate pushdown, buffer
> compaction). Without automated benchmarks in CI, performance regressions
> will slip through silently. This adds a benchmark suite that runs on every
> PR and compares against a committed baseline — any statistically significant
> regression blocks the merge.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BENCH-CI-1 | **Benchmark harness in CI.** Run `just bench` (Criterion-based) on a fixed hardware profile (GitHub Actions large runner or self-hosted). Capture results as JSON artifacts. Compare against committed baseline using Criterion's `--save-baseline` / `--baseline`. | 2–3d | [plans/performance/PLAN_PERFORMANCE_PART_9.md §I](plans/performance/PLAN_PERFORMANCE_PART_9.md) |
| BENCH-CI-2 | **Regression gate.** Parse Criterion JSON output; fail CI if any benchmark regresses by more than 10% (configurable threshold). Report regressions as PR comment with before/after numbers. | 1–2d | [plans/performance/PLAN_PERFORMANCE_PART_9.md §I](plans/performance/PLAN_PERFORMANCE_PART_9.md) |
| BENCH-CI-3 | **Scenario coverage.** Ensure benchmark suite covers: scan, filter, aggregate (algebraic + non-algebraic), join (2-table, 3-table), window function, CTE, TopK, append-only, and mixed workloads. At minimum 1K/10K/100K row scales. | 2–3d | [plans/performance/PLAN_PERFORMANCE_PART_9.md §I](plans/performance/PLAN_PERFORMANCE_PART_9.md) |

> **BENCH-CI subtotal: ~1–2 weeks**

### Auto-Indexing on Stream Table Creation (AUTO-IDX)

> **In plain terms:** pg_ivm automatically creates indexes on GROUP BY columns
> and primary key columns when creating an incrementally maintained view.
> pg_trickle currently requires manual index creation, which is a friction
> point for new users. Auto-indexing creates appropriate indexes at stream
> table creation time — GROUP BY keys, DISTINCT columns, and the
> `__pgt_row_id` covering index for MERGE performance.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~AUTO-IDX-1~~ | ~~**Auto-create indexes on GROUP BY / DISTINCT columns.**~~ ✅ GROUP BY composite index (existing) and DISTINCT composite index (new) auto-created at `create_stream_table()` time. Gated behind `pg_trickle.auto_index` GUC. | — | [src/api.rs](src/api.rs) |
| ~~AUTO-IDX-2~~ | ~~**Covering index on `__pgt_row_id`.**~~ ✅ Already implemented (A-4). Now gated behind `pg_trickle.auto_index` GUC (default `true`). | — | [src/api.rs](src/api.rs) |

> **AUTO-IDX: ✅ Done**

### Quick Wins

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~C2-BUG~~ | ~~**Implement missing `resume_stream_table()`.**~~ ✅ Already existed since v0.2.0 — verified operational. | — | |
| ~~ERR-REF~~ | ~~**Error reference documentation.**~~ ✅ Published as `docs/ERRORS.md` with all 20 variants documented. Cross-linked from FAQ. | — | [docs/ERRORS.md](docs/ERRORS.md) |
| ~~GUC-DEFAULTS~~ | ~~**Review dangerous GUC defaults.**~~ ✅ Defaults kept at `true` (correct for most workloads). Added detailed tuning guidance for memory-constrained and PgBouncer environments in CONFIGURATION.md. | — | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| ~~BUF-LIMIT~~ | ~~**Change buffer hard growth limit.**~~ ✅ `pg_trickle.max_buffer_rows` GUC added (default: 1M). Forces FULL refresh + truncation when exceeded. | — | [src/config.rs](src/config.rs) · [src/refresh.rs](src/refresh.rs) |

> **Quick wins: ✅ Done**

> **v0.16.0 total: ~1–2 weeks (MERGE alts) + ~4–6 weeks (aggregate fast-path) + ~1–2 weeks (append-only) + ~2–3 weeks (predicate pushdown) + ~2–3 weeks (template cache) + ~2–3 weeks (buffer compaction) + ~3–6 weeks (test coverage) + ~1–2 weeks (bench CI) + ~2–3 days (auto-indexing) + ~2–4 hours (quick wins)**
> *Note: PG 19 compatibility (A3, ~18–36h) moved to v1.0.0.*

**Exit criteria:**
- [x] PH-D1: DELETE+INSERT strategy implemented and gated behind `merge_strategy` GUC; correctness verified for INSERT/UPDATE/DELETE deltas
- [x] B-1: Algebraic aggregate fast-path replaces MERGE for `SUM`/`COUNT`/`AVG` GROUP BY queries; `aggregate_fast_path` GUC respected; explicit DML path (DELETE+UPDATE+INSERT) used instead of MERGE for all-algebraic aggregates; `explain_st()` exposes `aggregate_path`; existing tests pass — ✅ Done in v0.16.0 Phase 8
- [x] A-3-AO: `CREATE STREAM TABLE … APPEND ONLY` accepted; refresh uses INSERT path; heuristic auto-promotion on insert-only buffers; falls back to MERGE on first non-insert CDC event
- [x] B-2: Delta predicate pushdown implemented for single-source Filter nodes (P2-7); DELETE correctness verified (OR old_col predicate); selective-query benchmarks show delta row reduction
- [x] G14-SHC: Cross-backend template cache eliminates cold-start; catalog-backed L2 cache with `template_cache` GUC; invalidation on DDL; `explain_st()` exposes stats
- ~~A3: PG 19 builds and passes full E2E suite~~ — moved to v1.0.0
- [x] C-4: Change buffer compaction reduces buffer size by ≥50% for high-churn workloads; `compact_threshold` GUC respected; no correctness regressions
- [x] TG2-WIN: Window function DVM execution tests cover ROW_NUMBER, RANK, DENSE_RANK, LAG/LEAD across INSERT/UPDATE/DELETE
- [x] TG2-JOIN: Join multi-cycle tests cover INNER/LEFT/FULL JOIN with UPDATE and DELETE propagation; no silent data loss
- [x] TG2-EQUIV: Differential ≡ Full equivalence validated for joins, aggregates, and window functions
- [x] TG2-MERGE: refresh.rs MERGE template generation has unit test coverage (completed in v0.17.0)
- [x] TG2-CANCEL: Timeout and cancellation during refresh tested; no resource leaks (completed in v0.17.0)
- [x] TG2-SCHEMA: Source table type changes and column renames tested end-to-end
- [x] BENCH-CI: Performance regression CI runs on every PR; 10% regression threshold blocks merge; scenario coverage includes scan/filter/aggregate/join/window/CTE/TopK/SemiJoin/AntiJoin
- [x] AUTO-IDX: Stream tables auto-create indexes on GROUP BY / DISTINCT columns; `__pgt_row_id` covering index for ≤ 8-column tables; `auto_index` GUC respected
- [x] C2-BUG: `resume_stream_table()` verified operational (present since v0.2.0)
- [x] ERR-REF: Error reference doc published with all 20 PgTrickleError variants, common causes, and suggested fixes
- [x] GUC-DEFAULTS: `planner_aggressive` and `cleanup_use_truncate` defaults reviewed; trade-offs documented in CONFIGURATION.md
- [x] BUF-LIMIT: `max_buffer_rows` GUC prevents unbounded change buffer growth; triggers FULL + truncation when exceeded
- [x] Extension upgrade path tested (`0.15.0 → 0.16.0`)
- [x] `just check-version-sync` passes

</details>

</details>

---

## v0.17.0 — Query Intelligence & Stability

**Status: Released (2026-04-08).**

**Goal:** Make the refresh engine smarter, prove correctness through automated
fuzzing, harden for scale, and prepare for adoption. Cost-based strategy
selection replaces the fixed DIFF/FULL threshold, columnar change tracking
skips irrelevant columns in wide-table UPDATEs, SQLancer integration provides
automated semantic proving, incremental DAG rebuild supports 1000+ stream table
deployments, and unsafe block reduction continues the safety hardening toward
1.0. On the adoption side: `api.rs` modularization improves code maintainability,
a pg_ivm migration guide targets the largest potential adopter audience, a
failure mode runbook equips production teams, and a Docker Compose playground
provides a 60-second tryout experience.

### Cost-Based Refresh Strategy Selection (B-4)

> **In plain terms:** The current adaptive FULL/DIFFERENTIAL threshold is a
> fixed ratio (`differential_max_change_ratio` default 0.5). A join-heavy
> query may be better off with FULL at 5% change rate, while a scan-only
> query benefits from DIFFERENTIAL up to 80%. This replaces the fixed
> threshold with a cost model trained on each stream table's own refresh
> history — selecting the cheapest strategy per cycle automatically.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| B-4 | **Cost-based refresh strategy selection.** Collect per-ST statistics (`delta_row_count`, `merge_duration_ms`, `full_refresh_duration_ms`, `query_complexity_class`) from `pgt_refresh_history`. Fit a simple linear cost model. Before each refresh, compare `estimated_diff_cost(Δ)` vs `estimated_full_cost × safety_margin` and select the cheaper path. Cold-start heuristic (< 10 refreshes) falls back to existing fixed threshold. Gate behind `pg_trickle.refresh_strategy = 'auto'\|'differential'\|'full'` GUC. | 2–3 wk | [plans/performance/PLAN_NEW_STUFF.md §B-4](plans/performance/PLAN_NEW_STUFF.md) |

> **B-4 subtotal: ~2–3 weeks**

### Columnar Change Tracking (A-2-COL)

> **In plain terms:** When a source table UPDATE changes only 1 of 50 columns,
> the current CDC captures the entire row (old + new) and the delta query
> processes all columns. If the changed column is not referenced by the stream
> table's defining query, the entire refresh is wasted work. Columnar change
> tracking adds a per-column bitmask to CDC events so the delta query can skip
> irrelevant rows at scan time — a 50–90% reduction in delta volume for
> wide-table OLTP workloads.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-2-COL-1 | **CDC trigger bitmask.** Compute `changed_columns` bitmask (`old.col IS DISTINCT FROM new.col`) in the CDC trigger; store as `int8` or `bit(n)` alongside the change row. | 1–2 wk | [plans/performance/PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) |
| A-2-COL-2 | **Delta-scan column filtering.** At delta-query build time, consult the bitmask: skip rows where no referenced column changed; use lightweight UPDATE-only path when only projected columns changed (no join keys, no filter predicates, no aggregate keys). | 1–2 wk | [plans/performance/PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) |
| A-2-COL-3 | **Aggregate correction optimization.** For aggregates where only the aggregated value column changed (not GROUP BY key), emit a single correction row instead of delete-old + insert-new. | 3–5d | [plans/performance/PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) |

> **A-2-COL subtotal: ~3–4 weeks**

### Transactional IVM Phase 4 Remaining (A2)

> **In plain terms:** IMMEDIATE mode (same-transaction refresh) shipped in
> v0.2.0 using SQL-level statement triggers. Phase 4 completes the transition
> to lower-overhead C-level triggers and ENR-based transition tables — sharing
> the transition tuplestore directly between the trigger and the refresh engine
> instead of copying through a temp table. Also adds prepared statement reuse
> to eliminate repeated parse/plan overhead for the delta query.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~A2-ENR~~ | ~~**ENR-based transition tables.**~~ 🚫 **Deferred post-1.0** — requires raw `pg_sys` ENR tuplestore FFI not surfaced by pgrx; carries memory-corruption and `pg_upgrade` compatibility risk. Revisit after 1.0 stabilisation. | ~~12–18h~~ | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) §Phase 4 |
| ~~A2-CTR~~ | ~~**C-level triggers.**~~ 🚫 **Deferred post-1.0** — requires raw `CreateTrigger()` FFI not surfaced by pgrx; carries memory-corruption and `pg_upgrade` compatibility risk. Revisit after 1.0 stabilisation. | ~~12–18h~~ | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) §Phase 4 |
| ~~A2-PS~~ | ~~**Prepared statement reuse.**~~ ✅ **Already shipped** — `pg_trickle.use_prepared_statements` GUC (default `true`) implemented and wired in `refresh.rs`; parse/plan overhead eliminated on steady-state workloads. | ~~8–12h~~ | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) §Phase 4 |

> **A2 subtotal: 0h remaining** (A2-PS shipped; A2-ENR + A2-CTR deferred post-1.0)

### `ROWS FROM()` Support (A8)

> **In plain terms:** `ROWS FROM()` with multiple set-returning functions
> is a rarely-used SQL feature, but supporting it closes a coverage gap
> in the parser and DVM pipeline.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A8 | **`ROWS FROM()` with multiple SRF functions.** Parser + DVM support for `ROWS FROM(generate_series(...), unnest(...))` in defining queries. Very low demand. | ~1–2d | [PLAN_TRANSACTIONAL_IVM_PART_2.md](plans/sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) Task 2.3 |

> **A8 subtotal: ~1–2 days**

### SQLancer Fuzzing Integration (SQLANCER)

> **In plain terms:** pg_trickle's tests were written by the pg_trickle team,
> which means they share the same assumptions as the code. SQLancer is an
> automated database testing tool that generates random SQL queries and checks
> whether the results are correct — it has found hundreds of bugs in
> PostgreSQL, SQLite, CockroachDB, and TiDB. Integrating SQLancer gives
> pg_trickle a crash-test oracle (does the parser panic on fuzzed input?),
> an equivalence oracle (does DIFFERENTIAL mode produce the same answer as
> FULL?), and stateful DML fuzzing (do random INSERT/UPDATE/DELETE sequences
> corrupt stream table data?). This is the single highest-value testing
> investment for finding unknown correctness bugs.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~SQLANCER-1~~ | ~~**Fuzzing environment.**~~ ✅ **Done** — Docker-based harness (`just sqlancer`), Rust LCG query generator, `SQLANCER_CASES`/`SQLANCER_SEED` controls, `weekly-sqlancer` CI job. | ~~2–3d~~ | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §1 |
| ~~SQLANCER-2~~ | ~~**Crash-test oracle.**~~ ✅ **Done** — `test_sqlancer_crash_oracle` / `run_crash_oracle()` verifies zero backend crashes over 200–2000 fuzzed queries. | ~~3–5d~~ | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §2 |
| ~~SQLANCER-3~~ | ~~**Equivalence oracle.**~~ ✅ **Done** — `test_sqlancer_diff_vs_full_oracle` / `run_diff_vs_full_oracle()` creates DIFFERENTIAL + FULL stream tables, applies 4 DML mutations, and asserts count parity. Integrated into `test_sqlancer_ci_combined`. | ~~3–5d~~ | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §3 |
| ~~SQLANCER-4~~ | ~~**Stateful DML fuzzing.**~~ ✅ **Done** — `test_sqlancer_stateful_dml` / `run_stateful_dml_fuzzing()` runs `SQLANCER_MUTATIONS` (default 100, nightly 10 000) random INSERT/UPDATE/DELETE mutations with checkpoints every 50. CI: `weekly-sqlancer-stateful` job (`SQLANCER_MUTATIONS=10000`). | ~~3–5d~~ | [PLAN_SQLANCER.md](plans/testing/PLAN_SQLANCER.md) §4 |

> **SQLANCER subtotal: 0 remaining** (all four items shipped in v0.17.0)

### Incremental DAG Rebuild (C-2)

> **In plain terms:** When any DDL change occurs (e.g. `ALTER STREAM TABLE`,
> `DROP STREAM TABLE`), the entire dependency graph is rebuilt from scratch
> by querying `pgt_dependencies`. For 1000+ stream tables this becomes
> expensive — O(V+E) SPI queries. Incremental DAG maintenance records which
> specific stream table was affected and only re-sorts the affected subgraph,
> reducing the scheduler latency spike from ~50ms to ~1ms at scale.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C-2-1 | **Delta-based rebuild.** Record affected `pgt_id` in a bounded ring buffer in shared memory alongside `DAG_REBUILD_SIGNAL`. On overflow, fall back to full rebuild. | 1 wk | [plans/performance/PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |
| C-2-2 | **Incremental topological sort.** Add/remove only affected edges and vertices; re-run topological sort on the affected subgraph only. Cache the sorted schedule in shared memory. | 1–2 wk | [plans/performance/PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |

> **C-2 subtotal: ~2–3 weeks**

### Unsafe Block Reduction — Phase 6 (UNSAFE-R1/R2)

> **In plain terms:** pg_trickle achieved a 51% reduction in `unsafe` blocks
> (from ~1,300 to 641) in earlier releases. The remaining blocks are
> concentrated in well-documented field-accessor macros and standalone
> `is_a` type checks. Converting these to safe wrappers removes another
> 150–250 unsafe blocks with minimal risk — a meaningful safety improvement
> before 1.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| UNSAFE-R1 | **Safe field-accessor macros.** Replace `unsafe { (*node).field }` patterns with safe accessor functions. Estimated reduction: ~100–150 unsafe blocks. | 2–4h | [PLAN_REDUCED_UNSAFE.md §R1](plans/safety/PLAN_REDUCED_UNSAFE.md) |
| UNSAFE-R2 | **Safe `is_a` checks.** Convert standalone `unsafe { is_a(node, T_Foo) }` calls to safe wrapper functions. Estimated reduction: ~50–99 unsafe blocks. | 2–4h | [PLAN_REDUCED_UNSAFE.md §R2](plans/safety/PLAN_REDUCED_UNSAFE.md) |

> **UNSAFE-R1/R2 subtotal: ~4–8 hours**

### `api.rs` Modularization (API-MOD)

> **In plain terms:** `api.rs` is 9,413 lines — the largest file in the
> codebase. It contains stream table CRUD, ALTER QUERY, CDC management,
> bulk operations, diagnostics, and monitoring functions all in one file.
> The same treatment that `parser.rs` received in v0.15.0 (split from 21K
> lines into 5 sub-modules) is needed here. Zero behavior change — purely
> structural.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| API-MOD | **Split `src/api.rs` into sub-modules.** Proposed split: `api/create.rs` (create/drop/alter), `api/refresh.rs` (refresh entry points), `api/cdc.rs` (CDC management), `api/diagnostics.rs` (explain_st, health_check), `api/bulk.rs` (bulk_create), `api/mod.rs` (re-exports). Zero behavior change. | 1–2 wk | — |

> **API-MOD subtotal: ~1–2 weeks**

### pg_ivm Migration Guide (MIG-IVM)

> **In plain terms:** pg_ivm is the incumbent IVM extension with 1,400+
> GitHub stars and 4 years of production use. Many potential pg_trickle
> adopters are currently using pg_ivm. A step-by-step migration guide —
> mapping pg_ivm concepts to pg_trickle equivalents, with concrete SQL
> examples — removes the biggest adoption friction for this audience.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| MIG-IVM | **pg_ivm → pg_trickle migration guide.** Map: `create_immv()` → `create_stream_table()`; `refresh_immv()` → `refresh_stream_table()`; IMMEDIATE mode equivalence; aggregate coverage differences (5 vs 60+); GUC mapping; worked example migrating a real pg_ivm deployment. Publish as `docs/tutorials/MIGRATING_FROM_PG_IVM.md`. | 2–3d | [docs/research/PG_IVM_COMPARISON.md](docs/research/PG_IVM_COMPARISON.md) |

> **MIG-IVM subtotal: ~2–3 days**

### Failure Mode Runbook (RUNBOOK)

> **In plain terms:** Production teams need to know what happens when things
> go wrong — and what to do about it. This documents every failure mode
> pg_trickle can encounter (scheduler crash, WAL slot lag, OOM during
> refresh, disk full, replication slot conflict, stuck watermarks, circular
> convergence failure) with symptoms, diagnosis steps, and resolution
> procedures. Essential for on-call engineers.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| RUNBOOK | **Failure mode runbook.** Document: scheduler crash recovery, WAL decoder failures, OOM during refresh, disk-full behavior, replication slot conflicts, stuck watermarks, circular convergence timeout, CDC trigger failures, SUSPENDED state recovery, lock contention diagnosis. Include `health_check()` output interpretation and `explain_st()` troubleshooting. Publish as `docs/TROUBLESHOOTING.md`. | 3–5d | [docs/PRE_DEPLOYMENT.md](docs/PRE_DEPLOYMENT.md) |

> **RUNBOOK subtotal: ~3–5 days**

### Docker Quickstart Playground (PLAYGROUND)

> **In plain terms:** The fastest way to evaluate any database extension is
> to run it locally in 60 seconds. A `docker-compose.yml` with PostgreSQL +
> pg_trickle pre-installed, sample data (e.g. the org-chart from
> GETTING_STARTED.md), and a Jupyter notebook or pgAdmin web UI gives
> potential users a zero-friction tryout experience. This is the single
> most impactful thing for driving initial adoption.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PLAYGROUND | **Docker Compose quickstart.** `docker-compose.yml` with: PG 18 + pg_trickle, seed SQL script (org-chart example from GETTING_STARTED.md + TPC-H SF=0.01), pgAdmin web UI (optional). Single `docker compose up` command. README with guided walkthrough. | 2–3d | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |

> **PLAYGROUND subtotal: ~2–3 days**

### Documentation Polish (DOC-POLISH)

> **In plain terms:** The existing documentation is comprehensive and
> technically excellent, but it's optimized for users already familiar with
> IVM and PostgreSQL internals. These items restructure the docs for a
> better "first hour" experience — simpler getting-started examples, a
> refresh mode decision guide, a condensed new-user FAQ, and a setup
> verification checklist. The goal is to reduce cognitive overload for new
> users without losing the depth that experienced users need.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DOC-HELLO | **Simplified "Hello Stream Table" in GETTING_STARTED.** Add a Chapter 0 with a single-table, single-aggregate stream table (e.g. `SELECT department, count(*) FROM employees GROUP BY department`). Create it, insert a row, verify the refresh. Build confidence before the multi-table org-chart example. | 2–4h | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| DOC-DECIDE | **Refresh mode decision guide.** Flowchart: "Need transactional consistency? → IMMEDIATE. Volatile functions? → FULL. Otherwise → AUTO (DIFFERENTIAL with FULL fallback)." Include when-to-use guidance for each mode with concrete examples. Publish as a section in GETTING_STARTED or as a standalone tutorial. | 2–4h | [docs/tutorials/tuning-refresh-mode.md](docs/tutorials/tuning-refresh-mode.md) |
| DOC-FAQ-NEW | **New User FAQ (top 15 questions).** Extract the 15 most common new-user questions from the 3,000-line FAQ into a prominent "New User FAQ" section at the top. Keyword-rich headings for searchability. Link to deep FAQ for details. | 2–3h | [docs/FAQ.md](docs/FAQ.md) |
| DOC-VERIFY | **Post-install verification checklist.** SQL script that verifies: extension loaded, shared_preload_libraries configured, GUCs set, CDC triggers installable, first stream table creates and refreshes successfully. Runnable as `psql -f verify_install.sql`. | 2–4h | [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) |
| DOC-STUBS | **Fill or remove research stubs.** `PG_IVM_COMPARISON.md` (60 bytes) and `CUSTOM_SQL_SYNTAX.md` (57 bytes) are empty stubs. Either flesh them out (PG_IVM_COMPARISON can draw from the existing comparison data) or remove from SUMMARY.md. | 2–4h | [docs/research/](docs/research/) |

> **DOC-POLISH subtotal: ~2–3 days**

> **v0.17.0 total: ~2–3 weeks (cost-based strategy) + ~3–4 weeks (columnar tracking) + ~32–48 hours (TIVM Phase 4) + ~1–2 days (ROWS FROM) + ~2–3 weeks (SQLancer) + ~2–3 weeks (incremental DAG) + ~4–8 hours (unsafe reduction) + ~1–2 weeks (api.rs modularization) + ~2–3 days (pg_ivm migration) + ~3–5 days (failure runbook) + ~2–3 days (Docker playground) + ~2–3 days (doc polish)**

**Exit criteria:**
- [x] B-4: Cost-based strategy selector trained on per-ST history; cold-start fallback to fixed threshold; `QueryComplexityClass` cost model (scan/filter/aggregate/join/join_agg); `refresh_strategy` + `cost_model_safety_margin` GUCs; pre-refresh predictive comparison; 10 unit tests
- [x] A-2-COL: CDC trigger emits `changed_cols` VARBIT bitmask (COL-1); delta-scan filters irrelevant rows via `changed_cols & mask` (COL-2); aggregate value-only correction 'V' path halves row volume (COL-3)
- ~~[ ] A2-ENR~~: 🚫 Deferred post-1.0 — requires raw `pg_sys` ENR tuplestore FFI (memory-corruption risk); revisit after 1.0 stabilisation
- ~~[ ] A2-CTR~~: 🚫 Deferred post-1.0 — requires raw `CreateTrigger()` C FFI (memory-corruption risk); revisit after 1.0 stabilisation
- [x] A2-PS: ✅ Already shipped — `pg_trickle.use_prepared_statements` GUC (default `true`) wired in `refresh.rs`; parse/plan overhead eliminated on steady-state workloads
- [x] A8: `ROWS FROM()` with multiple SRFs accepted in defining queries; E2E tests cover INSERT/UPDATE/DELETE propagation
- [x] SQLANCER: ✅ SQLANCER-1/2 crash + equivalence oracles shipped in v0.12.0; SQLANCER-3 diff-vs-full oracle and SQLANCER-4 stateful DML soak (10K mutations) added in v0.17.0; `weekly-sqlancer-stateful` CI job wired
- [x] C-2: Incremental DAG rebuild reduces DDL-triggered latency spike to < 5ms at 100+ STs; ring buffer overflow falls back to full rebuild; no correctness regressions
- [x] UNSAFE-R1/R2: Unsafe block count reduced by 249 (690→441 in parser); `is_node_type!` and `pg_deref!` macros; all 1,700 unit tests pass
- [x] API-MOD: `api.rs` split into 3 sub-modules (mod.rs 5,624 + diagnostics.rs 1,377 + helpers.rs 2,461); zero behavior change; all 1,700 unit tests pass
- [x] MIG-IVM: `docs/tutorials/MIGRATING_FROM_PG_IVM.md` published with step-by-step migration, API mapping, behavioral differences, SQL upgrade examples, and verification checklist
- [x] RUNBOOK: `docs/TROUBLESHOOTING.md` covers 13 failure scenarios (scheduler, SUSPENDED, CDC triggers, WAL slots, INITIALIZING, buffer growth, lock contention, OOM, disk full, circular convergence, schema changes, worker pool, fuse) with symptoms, diagnosis, and resolution
- [x] PLAYGROUND: `playground/` with docker-compose.yml, seed.sql (3 base tables, 5 stream tables), and README walkthrough
- [x] DOC-HELLO: Chapter 1 "Hello World" in GETTING_STARTED already provides the single-table aggregate example (products/category_summary)
- [x] DOC-DECIDE: Refresh mode decision guide already published as `tutorials/tuning-refresh-mode.md` with `recommend_refresh_mode()` and signal breakdown
- [x] DOC-FAQ-NEW: New User FAQ section with 15 keyword-rich entries added at top of FAQ.md
- [x] DOC-VERIFY: `scripts/verify_install.sql` checks shared_preload_libraries, extension, scheduler, GUCs, and runs end-to-end stream table cycle
- [x] DOC-STUBS: Research stubs already use `{{#include}}` directives pointing to substantial content (923 + 1232 lines)
- [x] Extension upgrade path tested (`0.16.0 → 0.17.0`)

---

## v0.18.0 — Hardening & Delta Performance

> **Release Theme**
> This release hardens pg_trickle for production at scale and delivers the
> biggest remaining performance win in the differential refresh path. The Z-set
> multi-source delta engine merges per-source delta branches into a single
> `GROUP BY + SUM(weight)` query, eliminating redundant join evaluation when
> multiple source tables change in the same cycle. Cross-source snapshot
> consistency guarantees that multi-source stream tables always read all
> upstream tables at the same transaction boundary — closing the last known
> correctness gap. Every production-path `.unwrap()` is replaced with graceful
> error propagation, another ~69 unsafe blocks are eliminated, and a populated
> TPC-H baseline turns the 22-query suite into a true regression canary.
> SQLancer fuzzing integration provides an external, assumption-free
> correctness oracle. Together, these changes build the confidence foundation
> for 1.0.

### Correctness

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | Enforce cross-source snapshot consistency | L | P0 |
| CORR-2 | Populate TPC-H expected-output regression guard | XS | P0 |
| CORR-3 | NULL-safe GROUP BY elimination under deletes | S | P1 |
| CORR-4 | Z-set merged-delta weight accounting proof | M | P0 |
| CORR-5 | HAVING-filtered aggregate correction under group depletion | S | P1 |

**CORR-1 — Enforce cross-source snapshot consistency (CSS-3)**

> **In plain terms:** When a stream table reads from two different source
> tables, there is a window where it can see source A at a newer point in
> time than source B — for example, seeing a new order but the old
> inventory count. Phase 3 completes the tick-watermark enforcement so
> both sources are always read at the same consistent LSN before any
> refresh proceeds. Phases 1 and 2 are already complete.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| CSS-3-1 | LSN watermark enforcement in the scheduler — hold refresh until all upstream sources reach the same tick boundary | 4–6h | [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](plans/sql/PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) §Phase 3 |
| CSS-3-2 | Catalog column `pgt_css_watermark_lsn` + GUC `pg_trickle.cross_source_consistency` (default `off`) | 2–3h | — |
| CSS-3-3 | E2E test: concurrent writes to two sources, assert stream table never sees a split snapshot | 2–3h | — |

> **CSS-3 subtotal: ~8–12 hours**
> Dependencies: None. Schema change: Yes.

**CORR-2 — Populate TPC-H expected-output regression guard (TPCH-BASE)**

> **In plain terms:** The TPC-H correctness tests run all 22 queries but
> the expected-output comparison guard was never populated — so the tests
> catch structural failures but not quiet result regressions. Populating
> the baseline turns the suite into a true correctness canary.

| Item | Description | Effort |
|------|-------------|--------|
| TPCH-BASE-1 | Run TPC-H suite once at known-good state; capture output | 30min |
| TPCH-BASE-2 | Populate comparison baseline in `e2e_tpch_tests.rs` line 89 (remove TODO); verify guard fires on a deliberate regression | 1h |

> **TPCH-BASE subtotal: ~1–2 hours**
> Dependencies: None. Schema change: No.

**CORR-3 — NULL-safe GROUP BY elimination under deletes**

> **In plain terms:** When all rows in a GROUP BY group are deleted and the
> grouping key contains NULLs, the differential engine must correctly remove
> the group. SQL's three-valued logic in `IS DISTINCT FROM` may cause delta
> weight miscounting for NULL keys.

Verify: E2E test with `GROUP BY nullable_col`, delete all group members,
assert zero rows remain in the stream table.
Dependencies: None. Schema change: No.

**CORR-4 — Z-set merged-delta weight accounting proof**

> **In plain terms:** Companion correctness gate for PERF-1 (B3-MERGE). The
> Z-set algebra requires that `SUM(weight)` across all merged branches for
> every primary key never produces a spurious net-positive or net-negative for
> a single join path.

Verify: property-based tests (proptest) asserting `merged_weights ==
individual_branch_sums` for randomly generated multi-source DAGs. All
existing B3-3 diamond-flow tests must pass unchanged.
Dependencies: PERF-1. Schema change: No.

**CORR-5 — HAVING-filtered aggregate correction under group depletion**

> **In plain terms:** When a HAVING-qualified group loses enough rows to no
> longer satisfy the predicate (e.g., `HAVING count(*) > 5` and 3 of 6 rows
> are deleted), the differential aggregate path must delete the stream table
> row rather than leaving a stale row matching the old HAVING predicate.

Verify: E2E test with HAVING + selective deletes crossing the threshold.
Dependencies: None. Schema change: No.

### Stability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| STAB-1 | Eliminate production-path .unwrap() calls | S | P0 |
| STAB-2 | unsafe block reduction Phase 1 | M | P1 |
| STAB-3 | Spill detection alerting | S | P1 |
| STAB-4 | Parallel worker orphaned resource cleanup | M | P1 |
| STAB-5 | Upgrade migration test (0.17→0.18) | S | P0 |
| STAB-6 | Error SQLSTATE coverage audit | S | P2 |

**STAB-1 — Eliminate production-path .unwrap() calls (SAFE-1)**

> **In plain terms:** A small number of SQL-parsing code paths in
> production (non-test) code call `.unwrap()` directly — if they encounter
> unexpected input they will panic the backend process and disconnect all
> clients. These should propagate errors gracefully instead.

| Item | Description | Effort |
|------|-------------|--------|
| SAFE-1-1 | `detect_and_strip_distinct()` call in `api.rs` (L8163) → propagate `PgTrickleError` | 1h |
| SAFE-1-2 | `find_top_level_keyword(sql, "FROM")` calls in `api.rs` (L8229–8258, 3×) → propagate error | 1h |
| SAFE-1-3 | `merge_sql[using_start.unwrap()..using_end.unwrap()]` in `refresh.rs` (L6236) → bounds-check | 1h |
| SAFE-1-4 | `entry.unwrap()` in delta computation loop in `refresh.rs` (L5992) → return `Err` | 1h |
| SAFE-1-5 | Chained `.unwrap().unwrap()` in `refresh.rs` (L6556–6557) → propagate | 1h |

> **SAFE-1 subtotal: ~4–6 hours**
> Dependencies: None. Schema change: No.

**STAB-2 — unsafe block reduction Phase 1 (UNSAFE-P1)**

> **In plain terms:** The DVM parser has 1,286 `unsafe` blocks — 98% of
> the total. Phase 1 introduces a single `pg_cstr_to_str()` safe helper
> that eliminates ~69 of the most mechanical ones: C-string-to-Rust
> conversions. No API or behavior change; pure safety improvement.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| UNSAFE-P1-1 | Implement `pg_cstr_to_str(ptr: *const c_char) -> &str` safe wrapper in `src/dvm/parser/mod.rs` | 1h | [PLAN_REDUCED_UNSAFE.md](plans/safety/PLAN_REDUCED_UNSAFE.md) §Phase 1 |
| UNSAFE-P1-2 | Replace ~69 `unsafe { CStr::from_ptr(...).to_str()... }` call-sites with the safe helper | 4–6h | — |
| UNSAFE-P1-3 | `unsafe_inventory.sh` baseline update + CI check | 1h | `scripts/unsafe_inventory.sh` |

> **UNSAFE-P1 subtotal: ~6–8 hours**
> Dependencies: None. Schema change: No.

**STAB-3 — Spill detection alerting (PH-E2)**

> **In plain terms:** The GUCs `pg_trickle.spill_threshold_blocks` and
> `pg_trickle.spill_consecutive_limit` already exist to configure spill
> budgets, but no alert fires when a refresh actually spills to disk. This
> adds an `AlertEvent::SpillThresholdExceeded` notification so operators
> know when large delta queries are hitting disk.

| Item | Description | Effort |
|------|-------------|--------|
| PH-E2-1 | Add `AlertEvent::SpillThresholdExceeded` variant to `src/monitor.rs` | 1h |
| PH-E2-2 | Detect spill after MERGE execution; emit alert when consecutive count exceeds limit | 2–3h |
| PH-E2-3 | E2E test: configure low spill threshold, trigger spill, assert alert fires | 1–2h |

> **PH-E2 subtotal: ~4–6 hours**
> Dependencies: None. Schema change: No.

**STAB-4 — Parallel worker orphaned resource cleanup**

> **In plain terms:** After a parallel worker panics mid-refresh, advisory
> locks, `__pgt_delta_*` temp tables, and partially-written change buffer
> rows may be left behind. The scheduler recovery path must clean these up.

Audit the recovery path to ensure: (a) advisory locks are released on next
scheduler tick, (b) temp tables are cleaned up, (c) change buffer rows are
not double-counted on retry. Verify: E2E test simulating worker crash via
`pg_terminate_backend()` followed by successful recovery.
Dependencies: None. Schema change: No.

**STAB-5 — Upgrade migration test (0.17→0.18)**
Extend the upgrade E2E test framework to cover the 0.17.0→0.18.0 migration
path and the three-version chain 0.16→0.17→0.18. Verify: catalog column
additions, new function signatures, existing stream tables survive, refresh
continues working post-upgrade.
Dependencies: All schema-changing items (CORR-1). Schema change: No.

**STAB-6 — Error SQLSTATE coverage audit**
Audit all `ereport!()` and `error!()` calls for SQLSTATE classification.
Ensure every user-facing error has a unique, documented SQLSTATE code that
connection poolers and application retry logic can pattern-match. Cross-
reference with `docs/ERRORS.md` for completeness.
Dependencies: None. Schema change: No.

### Performance

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| PERF-1 | Z-set multi-source delta engine | L | P0 |
| PERF-2 | Cost-based refresh strategy completion | L | P1 |
| PERF-3 | Zero-change source branch elision | M | P1 |
| PERF-4 | Columnar change tracking Phase 1 — CDC bitmask | L | P1 |
| PERF-5 | Index hint generation for MERGE target | S | P2 |

**PERF-1 — Z-set multi-source delta engine (B3-MERGE)**

> **In plain terms:** When a stream table joins multiple tables and more
> than one of those tables receives changes in the same scheduler cycle,
> the current engine generates one delta branch per source and stacks them
> in a `UNION ALL`. With this change those branches are merged into a
> single `GROUP BY + SUM(weight)` query using Z-set algebra, eliminating
> duplicate evaluation of shared join paths. B3-1 (branch pruning) and
> B3-3 (correctness proofs) are already done; this is the final payoff.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| B3-2-1 | Z-set merged-delta generation in `src/dvm/diff.rs` (`DiffEngine::diff_node()`) | 8–10h | [PLAN_MULTI_TABLE_DELTA_BATCHING.md](plans/performance/PLAN_MULTI_TABLE_DELTA_BATCHING.md) |
| B3-2-2 | Unit + property-based tests (existing B3-3 diamond-flow tests must pass unchanged) | 2–4h | — |
| B3-2-3 | Benchmark regression check against Part-8 baseline | 2h | — |

> **B3-MERGE subtotal: ~12–16 hours**
> Dependencies: CORR-4 (property tests must accompany). Schema change: No.

**PERF-2 — Cost-based refresh strategy completion (B-4 remainder)**

> **In plain terms:** *Deferred from v0.17.0.* The `refresh_strategy` GUC
> landed in the current cycle. The remaining work is the per-ST cost model:
> collect `delta_row_count`, `merge_duration_ms`, `full_refresh_duration_ms`
> from `pgt_refresh_history`; fit a simple linear cost model; cold-start
> heuristic (<10 refreshes) falls back to the fixed threshold.

Verify: mixed-workload benchmark showing the model picks the cheaper strategy
≥80% of the time.
Dependencies: B-4 Phase 1 (shipped). Schema change: No.

**PERF-3 — Zero-change source branch elision**

> **In plain terms:** When building a multi-source delta query, skip branches
> entirely for sources with empty change buffers. Currently all branches are
> generated and executed regardless of whether a source has changes.

Verify: benchmark showing latency reduction when 1-of-3 sources changes vs.
all 3 changing.
Dependencies: PERF-1 (applies to the merged delta builder). Schema change: No.

**PERF-4 — Columnar change tracking Phase 1 — CDC bitmask (A-2-COL-1)**

> **In plain terms:** *Deferred from v0.17.0.* Compute `changed_columns`
> bitmask (`old.col IS DISTINCT FROM new.col`) in the CDC trigger; store as
> `int8` or `bit(n)` alongside the change row. Phase 1 only: bitmask
> computation + storage. Phase 2 (delta-scan filtering using the bitmask)
> deferred to v0.19.0. Provides the foundation for 50–90% delta volume
> reduction on wide-table UPDATE workloads.

Gate behind `pg_trickle.columnar_tracking` GUC (default `off`).
Dependencies: None. Schema change: Yes (change buffer schema addition).

**PERF-5 — Index hint generation for MERGE target**

> **In plain terms:** When the stream table has a covering index on the
> MERGE join keys, bias the planner toward the index to avoid expensive
> sequential scans during delta application on large stream tables.

Emit `SET enable_seqscan = off` within the MERGE statement's session.
Verify: `EXPLAIN ANALYZE` shows index scan on MERGE for tables with PK index.
Dependencies: None. Schema change: No.

### Scalability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| SCAL-1 | Change buffer growth stress test at 10× write rate | M | P1 |
| SCAL-2 | Parallel worker utilization profiling at 200+ STs | M | P2 |
| SCAL-3 | Delta working-set memory cap | M | P2 |

**SCAL-1 — Change buffer growth stress test at 10× write rate**
Run a sustained write load at 10× normal throughput for 30+ minutes with
intentionally slow refresh intervals. Verify the `max_buffer_rows` cap
triggers correctly, FULL refresh clears the backlog, no disk exhaustion
occurs, and the extension recovers cleanly once write rate normalizes. This
validates the v0.16.0 buffer growth protection under extreme conditions.
Dependencies: None. Schema change: No.

**SCAL-2 — Parallel worker utilization profiling at 200+ STs**
Profile the scheduler with 200+ stream tables across
`pg_trickle.max_workers` = 4/8/16 settings. Measure: CPU utilization per
worker, scheduling queue depth, per-ST refresh latency P50/P99. Identify
whether the scheduling loop itself becomes a bottleneck before worker
saturation. Document findings as a scaling guide section.
Dependencies: None. Schema change: No.

**SCAL-3 — Delta working-set memory cap**
The current delta merge can allocate unbounded `work_mem` for hash joins. Add
a configurable cap (`pg_trickle.delta_work_mem_mb`, default: 256 MB) that
triggers FULL refresh fallback when the delta working set would exceed the
limit, preventing OOM on unexpectedly large deltas. Verify: E2E test with low
cap triggers fallback and logs a warning.
Dependencies: None. Schema change: No.

### Ease of Use

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| UX-1 | Template cache observability | S | P1 |
| UX-2 | Pre-built Grafana dashboard panels | M | P1 |
| UX-3 | Error message actionability audit | S | P1 |
| UX-4 | Single-endpoint health summary function | S | P2 |
| UX-5 | Prometheus metric completeness audit | XS | P2 |
| UX-6 | TUI surfaces for cache_stats and health_summary | XS | P2 |

**UX-1 — Template cache observability (CACHE-OBS)**

> **In plain terms:** The delta SQL template cache (`IVM_DELTA_CACHE`)
> saves regenerating delta queries on every refresh cycle, but its hit rate
> is invisible to operators. Adding `pgtrickle.cache_stats()` lets you see
> whether the cache is effective and tune `pg_trickle.ivm_cache_size`
> accordingly.

| Item | Description | Effort |
|------|-------------|--------|
| CACHE-OBS-1 | Add hit/miss/eviction counters to `IVM_DELTA_CACHE` | 1h |
| CACHE-OBS-2 | Expose via `pgtrickle.cache_stats()` returning `(hits BIGINT, misses BIGINT, evictions BIGINT, size INT)` | 1–2h |
| CACHE-OBS-3 | Documentation and E2E smoke test | 1h |

> **CACHE-OBS subtotal: ~3–4 hours**
> Dependencies: None. Schema change: No.

**UX-2 — Pre-built Grafana dashboard panels**
Extend `monitoring/grafana/` with import-ready JSON panels for: refresh
latency P50/P99 histogram, differential vs. FULL refresh ratio over time,
change buffer backlog per stream table, spill event count, template cache hit
rate, and worker utilization gauge. Document import instructions in
`monitoring/README.md`.
Dependencies: UX-1 (cache stats metric), STAB-3 (spill events). Schema change: No.

**UX-3 — Error message actionability audit**
Audit all `PgTrickleError` variants and `ereport!()`/`error!()` calls. Ensure
every user-facing error includes: the stream table name (when applicable), the
operation that failed, and a 1-sentence remediation hint. Cross-reference with
`docs/ERRORS.md`; add missing entries.
Dependencies: None. Schema change: No.

**UX-4 — Single-endpoint health summary function**
New `pgtrickle.health_summary()` function returning a single-row JSONB:
total STs, healthy/degraded/error counts, oldest un-refreshed age, largest
buffer backlog, fuse status, scheduler state. Useful for monitoring
integrations (Nagios, Datadog) without parsing multiple views.
Dependencies: None. Schema change: No.

**UX-5 — Prometheus metric completeness audit**
Verify every metric emitted by the extension matches the documented name in
`docs/CONFIGURATION.md` §Prometheus. Remove undocumented metrics or add
documentation. Ensure metric names follow Prometheus naming conventions
(`pgtrickle_*` prefix, snake_case, unit suffix).
Dependencies: None. Schema change: No.

**UX-6 — TUI surfaces for cache_stats and health_summary**

> **In plain terms:** The new `pgtrickle.cache_stats()` (UX-1) and
> `pgtrickle.health_summary()` (UX-4) functions are useful in isolation but
> are most discoverable when surfaced in the TUI. Even a read-only status
> panel showing total STs, healthy/degraded/error counts, cache hit rate, and
> scheduler state would make these endpoints visible to users who reach the
> extension through `pgtrickle-tui` rather than raw SQL. Audit `pgtrickle-tui/src/`
> to identify the lightest-weight integration point (likely a new "Health" tab
> or an expanded "Status" panel). If TUI changes are out of scope for this
> release, document the gap in `docs/TUI.md` so it is not silently deferred.

Verify: TUI displays non-zero cache stats and a valid health JSONB row after
at least one refresh cycle in the E2E playground environment.
Dependencies: UX-1, UX-4. Schema change: No.

### Test Coverage

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| TEST-1 | TPC-H regression baseline | XS | P0 |
| TEST-2 | SQLancer fuzzing — crash-test oracle | L | P1 |
| TEST-3 | CDC edge cases: NULL PKs, composite PKs, generated columns | M | P1 |
| TEST-4 | Property-based tests for Z-set merged delta | M | P0 |
| TEST-5 | Light E2E eligibility audit | S | P2 |
| TEST-6 | Three-version upgrade chain test (0.16→0.17→0.18) | S | P0 |
| TEST-7 | dbt integration regression coverage | S | P1 |

**TEST-1 — TPC-H regression baseline (TPCH-BASE)**
Same as CORR-2. Capture known-good outputs; verify guard fires on deliberate
regression.
Dependencies: None. Schema change: No.

**TEST-2 — SQLancer fuzzing — crash-test oracle**

> **In plain terms:** *Deferred from v0.17.0 (second time).* Scope reduced
> to crash-test oracle only for v0.18.0: SQLancer in Docker, configured to
> feed randomized SQL to the parser and DVM pipeline. Zero-panic guarantee —
> any input that crashes the extension is a bug. Equivalence oracle
> (DIFFERENTIAL ≡ FULL) and stateful DML fuzzing deferred to v0.19.0.

Verify: 10K+ fuzzed queries with zero panics.
Dependencies: None. Schema change: No.

**TEST-3 — CDC edge cases: NULL PKs, composite PKs, generated columns**
Create E2E tests covering: (a) tables with nullable PK columns in
differential mode, (b) composite PKs with 3+ columns, (c) `GENERATED ALWAYS
AS` stored columns as source columns, (d) domain-typed columns, (e)
array-typed columns referenced in defining queries.
Dependencies: None. Schema change: No.

**TEST-4 — Property-based tests for Z-set merged delta**
Required companion to PERF-1. proptest-based tests generating random
multi-source DAGs (2–5 sources, 1–3 join levels) with random DML sequences.
Assert merged delta produces identical stream table state as sequential
per-branch application. Detect weight-accounting bugs before they ship.
Dependencies: PERF-1. Schema change: No.

**TEST-5 — Light E2E eligibility audit**
Review all 10 full E2E test files (~90 tests). Identify tests that don't
require custom Docker image features (custom extensions, special
configurations) and can run on the stock `postgres:18.3` image. Migrate
eligible tests to reduce CI wall-clock time on PRs.
Dependencies: None. Schema change: No.

**TEST-6 — Three-version upgrade chain test (0.16→0.17→0.18)**
Extend upgrade E2E tests to cover: fresh install of 0.16.0, create stream
tables, upgrade to 0.17.0, verify survival, upgrade to 0.18.0, verify
survival + new features functional.
Dependencies: All schema-changing items. Schema change: No.

**TEST-7 — dbt integration regression coverage**

> **In plain terms:** The `dbt-pgtrickle` macro package is the primary
> adoption vector for teams using dbt, but the integration test suite in
> `dbt-pgtrickle/integration_tests/` currently verifies only happy-path macro
> expansion. Add regression tests covering: (a) `pgtrickle_stream_table` macro
> with all supported materialisation strategies (`differential`, `full`, `auto`),
> (b) incremental model compatibility, (c) `pgtrickle_status` test macro,
> (d) teardown and recreation idempotency (drop + re-run produces identical
> output). Run as part of `just test-dbt`.

Verify: `just test-dbt` passes all new cases; idempotency test confirms
identical stream table contents after a full `dbt run --full-refresh` cycle.
Dependencies: None. Schema change: No.

### Conflicts & Risks

1. **PERF-1 + CORR-4 + TEST-4 form a mandatory cluster.** The Z-set
   multi-source delta engine (B3-MERGE) is the highest-impact performance
   item but also touches the DVM engine core. Property-based tests (TEST-4)
   and the weight accounting proof (CORR-4) are *not optional* — they must
   ship alongside PERF-1 to prevent correctness regressions.

2. **Two schema changes.** CORR-1 (CSS-3) adds `pgt_css_watermark_lsn` to
   the catalog. PERF-4 (A-2-COL-1) adds `changed_columns` to change buffer
   tables. Both require upgrade migration scripts and freeze-risk
   coordination. Consider batching both into a single migration file.

3. **PERF-3 depends on PERF-1.** Zero-change branch elision modifies the
   same delta query builder as B3-MERGE. Sequence PERF-3 strictly after
   PERF-1 to avoid merge conflicts and compound risk.

4. **TEST-2 (SQLancer) is deferred for the second time.** Originally planned
   for v0.17.0, it remains unstarted. v0.18.0 scopes it to crash-test oracle
   only (L effort instead of XL), but there is a risk of perpetual deferral.
   If capacity is tight, prioritize the crash-test oracle as a standalone
   deliverable rather than deferring the full suite again.

5. **PERF-2 (cost model) requires production history data.** The per-ST cost
   model trains on `pgt_refresh_history`. Users upgrading from v0.17.0 will
   have a cold history cache. The cold-start heuristic (< 10 refreshes) is
   critical — test it explicitly.

6. **PERF-4 (columnar tracking) changes CDC trigger output.** The
   `changed_columns` bitmask adds overhead to every trigger invocation.
   Gate behind a GUC (default `off`) and benchmark the per-row overhead
   (`< 1μs` target) before enabling by default in a later release.

7. **B-4 and A-2-COL are carry-overs from v0.17.0.** Both were originally
   scoped for v0.17.0 but not started. They are re-proposed here with
   reduced scope (B-4 cost model only, A-2-COL Phase 1 bitmask only).
   If v0.17.0 ships B-4 partially, adjust PERF-2 scope accordingly.

> **v0.18.0 total: ~70–100 hours**

**Exit criteria:**
- [ ] CORR-1: Split-snapshot E2E test passes under concurrent writes; `pgt_css_watermark_lsn` column added
- [ ] CORR-2 / TEST-1: TPC-H baseline populated; deliberate regression detected by the guard
- [ ] CORR-3: NULL-keyed GROUP BY group fully removed after all-row delete
- [ ] CORR-4 / TEST-4: Property-based Z-set weight tests pass for randomly generated multi-source DAGs
- [ ] CORR-5: HAVING-qualified group deleted from stream table when row count drops below threshold
- [ ] STAB-1: All production-path `unwrap()` calls in `api.rs` and `refresh.rs` replaced with proper error propagation
- [ ] STAB-2: `unsafe_inventory.sh` reports ≥69 fewer `unsafe` blocks; CI baseline updated
- [ ] STAB-3: Spill alert fires in E2E test with artificially low threshold
- [ ] STAB-4: Worker crash recovery E2E test cleans up advisory locks, temp tables, and buffer rows
- [ ] STAB-5 / TEST-6: Three-version upgrade chain (0.16→0.17→0.18) passes
- [ ] STAB-6: All user-facing errors have documented SQLSTATE codes in `docs/ERRORS.md`
- [ ] PERF-1: Merged multi-source delta implemented; all B3-3 diamond-flow property tests pass unchanged
- [ ] PERF-2: Cost model picks cheaper strategy ≥80% of the time on mixed workload benchmark
- [ ] PERF-3: Zero-change branch elision shows measurable latency reduction in multi-source benchmark
- [ ] PERF-4: `changed_columns` bitmask stored in change buffer; per-row overhead < 1μs
- [ ] PERF-5: Index scan confirmed via EXPLAIN ANALYZE for MERGE on tables with PK covering index
- [ ] SCAL-1: Buffer growth stress test at 10× rate completes without disk exhaustion or data loss
- [ ] SCAL-2: Profiling report for 200+ STs documented
- [ ] SCAL-3: Delta work_mem cap triggers FULL fallback in E2E test
- [ ] UX-1: `pgtrickle.cache_stats()` returns correct counters in smoke test
- [ ] UX-2: Grafana dashboard JSON importable; documents refresh latency, buffer backlog, spill events
- [ ] UX-3: Error message audit complete; all errors include table name and remediation hint
- [ ] UX-4: `pgtrickle.health_summary()` returns single-row JSONB with correct counts
- [ ] UX-5: Prometheus metric names match documentation; no undocumented metrics
- [ ] TEST-2: SQLancer crash-test oracle runs 10K+ fuzzed queries with zero panics
- [ ] TEST-3: CDC edge case tests cover NULL PKs, composite PKs, generated columns, domain types, arrays
- [ ] TEST-5: At least 10 tests migrated from full E2E to light E2E
- [ ] TEST-7: dbt regression suite covers all macro strategies and teardown idempotency; `just test-dbt` passes
- [ ] UX-6: TUI (or `docs/TUI.md` gap note) reflects `cache_stats()` and `health_summary()` availability
- [ ] Extension upgrade path tested (`0.17.0 → 0.18.0`)
- [ ] `just check-version-sync` passes

---

## v0.19.0 — PGlite Proof of Concept

> **Release Theme**
> This release validates whether PGlite users want real incremental view
> maintenance by shipping a lightweight TypeScript plugin with zero core
> changes. The plugin (`@pgtrickle/pglite-lite`) intercepts DML via
> statement-level AFTER triggers and applies pre-computed delta SQL for
> simple patterns — single-table aggregates, two-table inner joins, and
> filtered scans. It deliberately limits scope to 3–5 SQL patterns to
> keep effort low while generating a concrete demand signal. If adoption
> materialises, the full core extraction (v0.20.0) and WASM build (v0.21.0)
> proceed. The main pg_trickle PostgreSQL extension ships no functional
> changes in this release — only version bumps and upgrade migration
> plumbing.

See [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) for the full
feasibility report.

### PGlite JS Plugin PoC (Strategy C — Phase 0)

> **In plain terms:** PGlite's built-in `live.incrementalQuery()` re-runs
> the full query on every change and diffs at the JavaScript layer. This
> proof of concept ships a PGlite plugin (`@pgtrickle/pglite-lite`) that
> intercepts DML via statement-level AFTER triggers and applies pre-computed
> delta SQL for simple cases — single-table aggregates and two-table inner
> joins. It validates whether PGlite users want real IVM and whether the
> trigger infrastructure works correctly in PGlite's single-user WASM mode.
> No WASM compilation, no pgrx changes, no core refactoring required.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PGL-0-1 | **PGlite trigger infrastructure validation.** Empirically verify that statement-level triggers with `REFERENCING NEW TABLE AS ... OLD TABLE AS ...` work in PGlite's single-user mode. Document any limitations. | 4–8h | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §8 Q1 |
| PGL-0-2 | **Delta SQL templates for simple patterns.** Implement delta SQL generation in TypeScript for: (a) single-table `GROUP BY` with `COUNT`/`SUM`/`AVG`, (b) two-table `INNER JOIN`, (c) simple `WHERE` filter. Pre-compute at `createStreamTable()` time. | 2–3d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy C |
| PGL-0-3 | **PGlite plugin skeleton.** TypeScript plugin implementing `createStreamTable()`, `dropStreamTable()`, trigger registration, and delta application via PGlite's plugin API. | 2–3d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy C |
| PGL-0-4 | **npm package `@pgtrickle/pglite-lite`.** Package, publish, README with usage examples, and 3–5 supported SQL patterns documented. | 1–2d | — |
| PGL-0-5 | **Benchmark vs `live.incrementalQuery()`.** Compare latency and throughput for a 10K-row table with single-row inserts. Quantify the IVM advantage. | 1d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §4.2 |

> **Phase 0 subtotal: ~2–3 weeks**

### Correctness

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | Delta SQL equivalence for supported patterns | M | P0 |
| CORR-2 | NULL-key aggregate correctness in JS delta | S | P0 |
| CORR-3 | Multi-DML transaction atomicity | S | P1 |

**CORR-1 — Delta SQL equivalence for supported patterns**

> **In plain terms:** The TypeScript delta SQL templates must produce the
> exact same stream table state as a full query re-evaluation, for every
> combination of INSERT, UPDATE, and DELETE on the supported patterns
> (single-table GROUP BY + COUNT/SUM/AVG, two-table INNER JOIN, simple
> WHERE filter). Correctness is proven by running each DML operation,
> comparing the delta-maintained result against a fresh `SELECT`, and
> asserting row-for-row equivalence.

Verify: automated test suite runs 100+ randomised DML sequences per pattern;
zero divergence from full re-evaluation.
Dependencies: PGL-0-2, PGL-0-3. Schema change: No.

**CORR-2 — NULL-key aggregate correctness in JS delta**

> **In plain terms:** When a GROUP BY key is NULL, SQL three-valued logic
> means `GROUP BY NULL` forms its own group. The TypeScript delta templates
> must handle NULL group keys correctly — insertions into the NULL group,
> deletions that empty it, and updates that move rows in/out of the NULL
> group. This is the most common correctness pitfall in hand-rolled IVM.

Verify: E2E test with nullable GROUP BY column; assert NULL group appears,
grows, shrinks, and disappears correctly.
Dependencies: CORR-1. Schema change: No.

**CORR-3 — Multi-DML transaction atomicity**

> **In plain terms:** PGlite runs in single-connection mode, so a
> `BEGIN; INSERT ...; DELETE ...; COMMIT` sequence fires two separate
> statement-level triggers. The plugin must ensure the stream table reflects
> the net effect of the entire transaction, not an intermediate state. If
> trigger ordering produces incorrect intermediate results, a
> post-transaction reconciliation pass is needed.

Verify: test with `BEGIN; INSERT; UPDATE; DELETE; COMMIT` on a single base
table; stream table matches full re-evaluation after commit.
Dependencies: PGL-0-3. Schema change: No.

### Stability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| STAB-1 | Trigger cleanup on dropStreamTable | S | P0 |
| STAB-2 | Graceful error on unsupported SQL | S | P0 |
| STAB-3 | Plugin idempotency (create-drop-create cycle) | S | P1 |

**STAB-1 — Trigger cleanup on dropStreamTable**

> **In plain terms:** When a user calls `dropStreamTable()`, all statement-
> level AFTER triggers registered on source tables must be removed. Orphaned
> triggers would fire on every subsequent DML and attempt to write to a
> non-existent stream table, causing errors.

Verify: after `dropStreamTable()`, no pg_trickle-related triggers remain in
`pg_trigger` for the source tables.
Dependencies: PGL-0-3. Schema change: No.

**STAB-2 — Graceful error on unsupported SQL**

> **In plain terms:** The PoC supports only 3–5 SQL patterns. If a user
> passes an unsupported query (e.g., a LEFT JOIN, window function, or
> recursive CTE), the plugin must throw a clear, actionable error message
> listing what is supported — not silently produce wrong results or crash.

Verify: `createStreamTable()` with an unsupported query throws an error
whose message names the unsupported feature and lists supported alternatives.
Dependencies: PGL-0-2. Schema change: No.

**STAB-3 — Plugin idempotency (create-drop-create cycle)**

> **In plain terms:** Creating a stream table, dropping it, and creating it
> again with the same name must work without leftover state. Leftover
> catalog rows, triggers, or temp tables from the first creation must not
> interfere with the second.

Verify: create-drop-create cycle produces correct results; no duplicate
triggers or stale catalog entries.
Dependencies: STAB-1. Schema change: No.

### Performance

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| PERF-1 | Benchmark vs live.incrementalQuery() | M | P0 |
| PERF-2 | Delta overhead profiling per DML | S | P1 |
| PERF-3 | Large result set scalability (10K/100K rows) | S | P1 |

**PERF-1 — Benchmark vs `live.incrementalQuery()`** (= PGL-0-5)

> **In plain terms:** The entire value proposition of this PoC depends on
> being faster than PGlite's built-in `live.incrementalQuery()` for the
> supported patterns. Produce a public benchmark comparing latency and
> throughput for single-row inserts into a 10K-row base table across all
> three supported patterns (aggregate, join, filter).

Verify: delta-maintained stream table refresh latency < 50% of
`live.incrementalQuery()` latency for all supported patterns at 10K rows.
Dependencies: PGL-0-3, PGL-0-4. Schema change: No.

**PERF-2 — Delta overhead profiling per DML**

> **In plain terms:** Measure the per-DML overhead added by the statement-
> level triggers. INSERT-heavy workloads should not suffer more than 2x
> latency increase compared to the same INSERT without pg_trickle triggers
> installed. Profile trigger function execution time, temp table creation,
> and delta DML.

Verify: microbenchmark shows per-DML overhead < 2 ms for aggregate pattern;
< 5 ms for join pattern at 10K source rows.
Dependencies: PGL-0-3. Schema change: No.

**PERF-3 — Large result set scalability (10K/100K rows)**

> **In plain terms:** Verify that the delta approach maintains its advantage
> over full re-evaluation as base table size grows. At 100K rows, the delta
> path should be significantly faster than full re-evaluation for single-row
> changes.

Verify: at 100K base table rows, single-row insert refresh latency is
< 10% of full query re-evaluation latency.
Dependencies: PERF-1. Schema change: No.

### Scalability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| SCAL-1 | Multiple stream tables on same source | S | P1 |
| SCAL-2 | Cascading stream table triggers | M | P2 |
| SCAL-3 | Concurrent DML with multiple stream tables | S | P2 |

**SCAL-1 — Multiple stream tables on same source**

> **In plain terms:** Verify that 3+ stream tables can be maintained from
> the same base table simultaneously. Each DML fires one trigger per stream
> table; ensure triggers do not interfere with each other.

Verify: 3 stream tables on the same source; INSERT + UPDATE + DELETE cycle;
all 3 produce correct results.
Dependencies: PGL-0-3. Schema change: No.

**SCAL-2 — Cascading stream table triggers**

> **In plain terms:** If stream table B reads from stream table A's
> underlying storage, an INSERT into A's source should propagate through
> A's trigger, update A, and then fire B's trigger to update B — all
> within the same PGlite transaction. Verify this works in PGlite's
> single-connection environment without deadlocks or infinite trigger loops.

Verify: A->B cascade produces correct results for INSERT/DELETE on A's
source. No infinite loops detected.
Dependencies: SCAL-1. Schema change: No.

**SCAL-3 — Concurrent DML with multiple stream tables**

> **In plain terms:** PGlite is single-connection, but a user could issue
> rapid sequential DML (`INSERT; INSERT; INSERT`) without explicit
> transactions. Verify all stream tables converge to the correct state.

Verify: 100 sequential INSERTs with 3 stream tables; final state matches
full re-evaluation.
Dependencies: SCAL-1. Schema change: No.

### Ease of Use

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| UX-1 | Getting-started README with copy-paste examples | S | P0 |
| UX-2 | Supported patterns decision table | XS | P0 |
| UX-3 | Error messages include remediation hints | S | P1 |
| UX-4 | TypeScript type definitions | S | P1 |
| UX-5 | ElectricSQL outreach and collaboration | S | P1 |

**UX-1 — Getting-started README with copy-paste examples**

> **In plain terms:** The npm package README must include 3 complete,
> copy-pasteable examples — one per supported pattern — that a developer
> can run in under 2 minutes. Include Node.js and browser (Vite) examples.

Verify: all README examples execute without modification on a fresh PGlite
instance.
Dependencies: PGL-0-4. Schema change: No.

**UX-2 — Supported patterns decision table**

> **In plain terms:** A clear table showing which SQL patterns are and are
> not supported, what error you get for unsupported patterns, and when full
> support is expected (v0.21.0). This prevents user frustration and sets
> expectations.

Verify: decision table in README and npm page lists all tested patterns with
status (supported / unsupported / planned).
Dependencies: None. Schema change: No.

**UX-3 — Error messages include remediation hints**

> **In plain terms:** Every error thrown by the plugin must include the
> table name, the failing operation, and a one-sentence hint. Example:
> `"LEFT JOIN is not supported in pglite-lite. Use @pgtrickle/pglite
> (v0.21.0+) for full SQL support, or rewrite as INNER JOIN."`

Verify: all error paths tested; every error message includes a remediation
sentence.
Dependencies: STAB-2. Schema change: No.

**UX-4 — TypeScript type definitions**

> **In plain terms:** Ship `.d.ts` type definitions so TypeScript users
> get autocomplete and type checking for `createStreamTable()`,
> `dropStreamTable()`, and configuration options.

Verify: TypeScript project consumes the plugin with strict mode; no `any`
types leaked.
Dependencies: PGL-0-4. Schema change: No.

**UX-5 — ElectricSQL outreach and collaboration**

> **In plain terms:** PGlite is developed by ElectricSQL. Their cooperation
> is essential for Phase 2 (WASM build). Initiate contact before shipping
> Phase 0 to gauge interest, validate assumptions about PGlite's trigger
> infrastructure, and explore potential co-marketing.

Verify: documented exchange with ElectricSQL team (GitHub issue, email, or
meeting notes).
Dependencies: None. Schema change: No.

### Test Coverage

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| TEST-1 | Automated correctness suite (all patterns x DML types) | M | P0 |
| TEST-2 | PGlite version compatibility matrix | S | P1 |
| TEST-3 | Regression test: trigger firing order | S | P1 |
| TEST-4 | Bundle size monitoring | XS | P2 |
| TEST-5 | Extension upgrade path (0.18 to 0.19) | S | P0 |

**TEST-1 — Automated correctness suite (all patterns x DML types)**

> **In plain terms:** For each supported pattern (aggregate, join, filter),
> run every DML type (INSERT, UPDATE, DELETE, multi-row, TRUNCATE) and
> assert the stream table matches a fresh full evaluation. This is the
> primary quality gate.

Verify: Jest/Vitest test suite with > 50 test cases; all pass on PGlite
latest.
Dependencies: PGL-0-2, PGL-0-3. Schema change: No.

**TEST-2 — PGlite version compatibility matrix**

> **In plain terms:** PGlite updates frequently. Test the plugin against
> the last 3 PGlite releases to ensure trigger behavior hasn't changed.
> Document the minimum supported PGlite version.

Verify: CI matrix runs tests against PGlite N, N-1, N-2.
Dependencies: TEST-1. Schema change: No.

**TEST-3 — Regression test: trigger firing order**

> **In plain terms:** When multiple triggers exist on the same table,
> PostgreSQL fires them in alphabetical order by trigger name. Verify that
> trigger naming conventions prevent ordering conflicts with user-defined
> triggers.

Verify: test with a user-defined AFTER trigger alongside the plugin's
trigger; both fire correctly; stream table produces correct results.
Dependencies: PGL-0-3. Schema change: No.

**TEST-4 — Bundle size monitoring**

> **In plain terms:** The npm package should be small (< 50 KB minified +
> gzipped) since this is a pure-JS plugin with no WASM. Add a CI check
> that fails if bundle size exceeds the threshold.

Verify: `npm pack --dry-run` reports < 50 KB gzipped.
Dependencies: PGL-0-4. Schema change: No.

**TEST-5 — Extension upgrade path (0.18 to 0.19)**

> **In plain terms:** The main pg_trickle PostgreSQL extension ships no
> functional changes in v0.19.0, but the upgrade migration path must still
> be tested. `ALTER EXTENSION pg_trickle UPDATE` from 0.18.0 to 0.19.0
> must leave existing stream tables intact.

Verify: upgrade E2E test confirms all existing stream tables survive and
refresh correctly after `0.18.0 -> 0.19.0` upgrade.
Dependencies: None. Schema change: No (PG extension unchanged).

### Conflicts & Risks

1. **Demand uncertainty is the primary risk.** This entire milestone is a bet
   that PGlite users want IVM beyond what pg_ivm provides. If Phase 0
   generates no adoption signal, v0.20.0–v0.22.0 should be deprioritised and
   v1.0.0 proceeds without PGlite. Define a concrete adoption threshold
   (e.g., > 100 npm weekly downloads within 60 days of publication) as a
   go/no-go gate for v0.20.0.

2. **PGlite trigger infrastructure is unverified.** PGL-0-1 (trigger
   validation) is a hard prerequisite for everything else. If statement-level
   triggers with transition tables do not work in PGlite's single-user mode,
   the entire Strategy C approach fails and the PoC must pivot to a pure JS
   diff approach (lower value).

3. **PGlite version mismatch.** PGlite tracks PostgreSQL 17; pg_trickle
   targets PG 18. The PoC operates at the SQL level and should be unaffected,
   but if PGlite upgrades to PG 18 mid-cycle, trigger behavior may change.
   Pin the minimum PGlite version in `package.json`.

4. **No core Rust changes, but version bump required.** The main pg_trickle
   extension needs a v0.19.0 version bump, upgrade migration SQL, and passing
   CI even though no functional code changes. This is low-risk but must not
   be forgotten.

5. **ElectricSQL collaboration timing.** UX-5 (outreach) should happen
   early — before v0.19.0 ships — to avoid building something ElectricSQL is
   already working on or would actively resist. If they signal interest in
   co-development, Phase 2 scope and timeline may shift.

6. **TypeScript delta SQL correctness is harder to prove than Rust.** The
   main extension uses property-based testing and SQLancer for correctness.
   The TS plugin lacks these tools. TEST-1 must be rigorously designed to
   compensate — consider porting the proptest approach to a JS property-
   testing library (e.g., fast-check).

> **v0.19.0 total: ~2–3 weeks (PGlite plugin) + ~1–2 days (PG extension version bump)**

**Exit criteria:**
- [ ] PGL-0-1: Statement-level triggers with transition tables confirmed working in PGlite
- [ ] PGL-0-2: Delta SQL correct for single-table aggregate, two-table join, and filtered query
- [ ] PGL-0-3: `@pgtrickle/pglite-lite` plugin creates and maintains stream tables in PGlite
- [ ] PGL-0-4: npm package published with README and usage examples
- [ ] PGL-0-5: Benchmark shows measurable latency improvement over `live.incrementalQuery()` for supported patterns
- [ ] CORR-1: Automated delta SQL equivalence tests pass (100+ DML sequences per pattern)
- [ ] CORR-2: NULL-key aggregate groups correctly created, updated, and removed
- [ ] CORR-3: Multi-DML transaction produces correct net result
- [ ] STAB-1: No orphaned triggers after `dropStreamTable()`
- [ ] STAB-2: Unsupported SQL patterns produce clear, actionable errors
- [ ] STAB-3: Create-drop-create cycle produces correct results
- [ ] PERF-1: Delta refresh latency < 50% of `live.incrementalQuery()` at 10K rows
- [ ] PERF-3: Delta advantage holds at 100K rows (< 10% of full re-evaluation latency)
- [ ] SCAL-1: 3+ stream tables on same source produce correct results
- [ ] UX-1: README examples run unmodified on fresh PGlite instance
- [ ] UX-2: Supported patterns decision table published
- [ ] UX-4: TypeScript type definitions ship with strict-mode compatibility
- [ ] TEST-1: > 50 correctness test cases pass on PGlite latest
- [ ] TEST-2: CI tests pass against PGlite N, N-1, N-2
- [ ] TEST-5: Extension upgrade path tested (`0.18.0 -> 0.19.0`)
- [ ] `just check-version-sync` passes

---

## v0.20.0 — Core Extraction (`pg_trickle_core`)

> **Release Theme**
> This release surgically separates pg_trickle's "brain" — the DVM engine,
> operator delta SQL generation, query rewrite passes, and DAG computation —
> into a standalone Rust crate (`pg_trickle_core`) with zero pgrx dependency.
> The extraction touches ~51,000 lines of code across 30+ source files but
> produces zero user-visible behavior change: every existing test must pass
> unchanged. The payoff is threefold: the core crate compiles to WASM
> (enabling the PGlite extension in v0.21.0), pure-logic unit tests run
> without a PostgreSQL instance (10x faster CI), and the main extension
> gains a cleaner internal architecture. Approximately 500 unsafe blocks in
> the parser require an abstraction layer over raw `pg_sys` node traversal,
> making this the most technically demanding refactoring in the project's
> history.

See [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A for the
full extraction architecture.

### Core Crate Extraction (Phase 1)

> **In plain terms:** pg_trickle's "brain" — the code that analyses SQL
> queries, builds operator trees, and generates delta SQL — is currently
> tangled with pgrx (the Rust-to-PostgreSQL bridge). This milestone
> surgically separates the pure logic into its own crate so it can be
> compiled independently. The existing extension continues to work
> unchanged; it just imports from `pg_trickle_core` instead of having the
> code inline. A `trait DatabaseBackend` abstracts SPI and parser access
> so the core logic can be tested without a running PostgreSQL instance.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PGL-1-1 | **Create `pg_trickle_core` crate.** Workspace member with `[lib]` target, no pgrx dependency. Move `OpTree`, `Expr`, `Column`, `AggExpr`, and all shared types. | 1–2d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-2 | **Extract operator delta SQL generation.** Move all `src/dvm/operators/` logic (~24K lines, 23 files) into the core crate. Each operator's `generate_delta_sql()` becomes a pure function taking abstract types. | 3–5d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-3 | **Extract auto-rewrite passes.** Move view inlining, DISTINCT ON rewrite, GROUPING SETS expansion, and SubLink extraction into `pg_trickle_core::rewrites`. | 2–3d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-4 | **Extract DAG computation.** Move dependency graph, topological sort, cycle detection, diamond detection into `pg_trickle_core::dag`. | 1–2d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-5 | **Define `trait DatabaseBackend`.** Abstract trait for SPI queries and raw_parser access. Implement for pgrx in the main extension crate. | 2–3d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-6 | **WASM compilation gate.** Verify `pg_trickle_core` compiles to `wasm32-unknown-emscripten` target. CI check for WASM build. | 1–2d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-1-7 | **Existing test suite passes.** All unit, integration, and E2E tests pass with the refactored crate structure. Zero behavior change. | 2–3d | — |

> **Phase 1 subtotal: ~3–4 weeks**

### Correctness

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | Delta SQL output byte-for-byte equivalence | M | P0 |
| CORR-2 | OpTree serialization round-trip fidelity | S | P0 |
| CORR-3 | Rewrite pass ordering preservation | S | P1 |
| CORR-4 | DAG cycle detection parity after extraction | S | P1 |

**CORR-1 — Delta SQL output byte-for-byte equivalence**

> **In plain terms:** After the extraction, every operator's
> `generate_delta_sql()` must produce the exact same SQL string as it did
> before the refactoring. Any byte-level difference — even whitespace —
> indicates a semantic shift that could change query plans or correctness.
> Capture the SQL output for all 22 TPC-H stream tables before and after
> the extraction and assert bit-for-bit equality.

Verify: snapshot test comparing delta SQL for all TPC-H queries + the full
E2E test suite. Any diff fails the build.
Dependencies: PGL-1-2. Schema change: No.

**CORR-2 — OpTree serialization round-trip fidelity**

> **In plain terms:** The `OpTree` types are moving to a new crate. If any
> field is accidentally dropped or retyped during the move, the delta SQL
> generator will silently produce wrong output. Add a round-trip test:
> serialize an OpTree to JSON, deserialize it back, and assert structural
> equality. This catches missing `#[derive]` attributes and field ordering
> issues.

Verify: proptest generating random OpTrees; serialize-deserialize round-trip
produces identical trees.
Dependencies: PGL-1-1. Schema change: No.

**CORR-3 — Rewrite pass ordering preservation**

> **In plain terms:** The auto-rewrite passes (view inlining, DISTINCT ON,
> GROUPING SETS, SubLink extraction) must execute in the same order after
> extraction. Reordering could change the resulting OpTree and thereby the
> delta SQL. Add an integration test that runs all rewrite passes on a
> complex query (joining 3 tables with DISTINCT ON + GROUPING SETS) and
> asserts the final OpTree matches a golden snapshot.

Verify: golden-snapshot test for rewrite pass output on complex query.
Dependencies: PGL-1-3. Schema change: No.

**CORR-4 — DAG cycle detection parity after extraction**

> **In plain terms:** The cycle detection algorithm in `dag.rs` has subtleties
> around self-referencing views and diamond patterns. After moving to the
> core crate, the algorithm must detect the same cycles. Run the existing
> cycle-detection unit tests and add 3 new edge cases: self-referencing CTE,
> diamond with mixed IMMEDIATE/DIFFERENTIAL, and 4-level cascade.

Verify: all existing DAG unit tests pass + 3 new edge-case tests.
Dependencies: PGL-1-4. Schema change: No.

### Stability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| STAB-1 | pg_sys node abstraction layer (~500 unsafe blocks) | L | P0 |
| STAB-2 | Compile-time pgrx dependency leak detection | S | P0 |
| STAB-3 | Cargo workspace configuration correctness | S | P0 |
| STAB-4 | Extension upgrade path (0.19 to 0.20) | S | P0 |
| STAB-5 | Feature-flag isolation for WASM target | S | P1 |

**STAB-1 — pg_sys node abstraction layer (~500 unsafe blocks)**

> **In plain terms:** `rewrites.rs` (118 unsafe blocks, 295 `pg_sys` refs)
> and `sublinks.rs` (367 unsafe blocks, 492 `pg_sys` refs) are the most
> deeply coupled to pgrx. The core crate cannot contain raw `pg_sys` calls.
> Define a `trait NodeVisitor` (or equivalent) that wraps pg_sys node
> traversal behind safe method calls. The pgrx backend implements the trait
> using actual pg_sys pointers; a mock backend can be used for unit tests.
> This is the single highest-effort item in the release.

Verify: zero `pg_sys::` references in `pg_trickle_core/`; `grep -r pg_sys
pg_trickle_core/src/` returns empty.
Dependencies: PGL-1-1, PGL-1-5. Schema change: No.

**STAB-2 — Compile-time pgrx dependency leak detection**

> **In plain terms:** After extraction, any accidental `use pgrx::*` in the
> core crate would break the WASM build. Add a CI job that compiles
> `pg_trickle_core` in isolation (without the pgrx feature) and fails if any
> pgrx symbol is referenced. This catches leaks immediately rather than at
> WASM build time.

Verify: `cargo build -p pg_trickle_core --no-default-features` succeeds in
CI.
Dependencies: PGL-1-1. Schema change: No.

**STAB-3 — Cargo workspace configuration correctness**

> **In plain terms:** Adding a workspace member changes `Cargo.lock`
> resolution, feature unification, and `cargo pgrx` behavior. Verify:
> `cargo pgrx package` still produces a valid `.so`, `cargo test` runs all
> workspace tests, and `cargo pgrx test` works for the extension crate.
> pgrx version must remain pinned at 0.17.x.

Verify: `cargo pgrx package`, `cargo test --workspace`, `cargo pgrx test`
all succeed.
Dependencies: PGL-1-1. Schema change: No.

**STAB-4 — Extension upgrade path (0.19 to 0.20)**

> **In plain terms:** v0.20.0 makes no SQL-visible changes (same functions,
> same catalog schema), but the upgrade migration must still be tested.
> `ALTER EXTENSION pg_trickle UPDATE` from 0.19.0 to 0.20.0 must leave
> existing stream tables intact and refreshable.

Verify: upgrade E2E test confirms stream tables survive and refresh
correctly after `0.19.0 -> 0.20.0`.
Dependencies: None. Schema change: No.

**STAB-5 — Feature-flag isolation for WASM target**

> **In plain terms:** The core crate must compile on both native and WASM.
> Any platform-specific code (e.g., `std::time::Instant` unavailable on
> `wasm32-unknown-emscripten`) must be gated behind `#[cfg]` attributes.
> Add a CI matrix entry for the WASM target that catches platform leaks.

Verify: `cargo build --target wasm32-unknown-emscripten -p pg_trickle_core`
succeeds in CI.
Dependencies: PGL-1-6. Schema change: No.

### Performance

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| PERF-1 | Zero-overhead abstraction for DatabaseBackend | M | P0 |
| PERF-2 | Benchmark regression gate across extraction | S | P0 |
| PERF-3 | Core-only unit test speedup measurement | S | P1 |

**PERF-1 — Zero-overhead abstraction for DatabaseBackend**

> **In plain terms:** The `trait DatabaseBackend` introduces dynamic
> dispatch (`dyn DatabaseBackend` or generics). For the native extension,
> the abstraction must add zero measurable overhead. Use monomorphization
> (generics, not trait objects) for the hot path — delta SQL generation is
> called on every refresh cycle and must not regress. Measure with Criterion
> before/after on the `diff_operators` benchmark suite.

Verify: Criterion benchmark shows < 1% regression on `diff_operators` suite
after extraction.
Dependencies: PGL-1-5. Schema change: No.

**PERF-2 — Benchmark regression gate across extraction**

> **In plain terms:** The extraction touches 51K lines of code. Even
> without functional changes, module restructuring can alter inlining,
> cache locality, and link-time optimization. Run the full Criterion
> benchmark suite before and after and assert no regression > 5%.

Verify: `scripts/criterion_regression_check.py` passes with 5% threshold on
all existing benchmarks.
Dependencies: PGL-1-7. Schema change: No.

**PERF-3 — Core-only unit test speedup measurement**

> **In plain terms:** One of the key benefits of extraction is that
> `pg_trickle_core` unit tests run without starting PostgreSQL. Measure the
> wall-clock time for `cargo test -p pg_trickle_core` vs the old in-tree
> unit tests. Document the speedup in the CHANGELOG — expect 5-10x faster
> CI for unit-level tests.

Verify: document test execution times before/after in PR description.
Dependencies: PGL-1-7. Schema change: No.

### Scalability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| SCAL-1 | Workspace build parallelism verification | S | P1 |
| SCAL-2 | Core crate binary size for WASM budget | S | P1 |
| SCAL-3 | Incremental compilation impact assessment | S | P2 |

**SCAL-1 — Workspace build parallelism verification**

> **In plain terms:** With two crates, `cargo build` can compile
> `pg_trickle_core` and other non-dependent crates in parallel. Verify that
> the workspace DAG allows parallel compilation and measure the
> incremental rebuild time for a change in `pg_trickle_core` only.

Verify: `cargo build --timings` shows parallel compilation of core crate.
Dependencies: PGL-1-1. Schema change: No.

**SCAL-2 — Core crate binary size for WASM budget**

> **In plain terms:** v0.21.0 targets < 2 MB WASM bundle. Measure the
> compiled size of `pg_trickle_core` for the WASM target now so the budget
> is known before Phase 2. If > 5 MB, investigate `wasm-opt` stripping and
> feature-gating large operator modules.

Verify: `wasm32-unknown-emscripten` build of `pg_trickle_core` produces < 5
MB unoptimized. Document size in tracking issue.
Dependencies: PGL-1-6. Schema change: No.

**SCAL-3 — Incremental compilation impact assessment**

> **In plain terms:** Splitting into two crates changes the incremental
> compilation boundary. A change in `pg_trickle_core` now forces a
> recompile of the extension crate. Measure incremental compile time for
> common edit patterns (add a test, modify an operator, change a rewrite
> pass) and ensure developer-experience compile times remain < 30s.

Verify: document incremental compile times for 3 edit patterns.
Dependencies: PGL-1-1. Schema change: No.

### Ease of Use

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| UX-1 | Workspace-aware justfile targets | S | P0 |
| UX-2 | Developer guide for core crate contributions | S | P1 |
| UX-3 | ARCHITECTURE.md update for two-crate layout | S | P1 |

**UX-1 — Workspace-aware justfile targets**

> **In plain terms:** Existing `just` targets (`just test-unit`, `just
> lint`, `just fmt`) must work seamlessly with the new workspace layout.
> Update the justfile so `just test-unit` runs both `pg_trickle_core` unit
> tests and extension unit tests. Add `just test-core` for core-only tests.

Verify: all existing `just` targets pass; `just test-core` runs core-only
tests in < 5 seconds.
Dependencies: PGL-1-1. Schema change: No.

**UX-2 — Developer guide for core crate contributions**

> **In plain terms:** Contributors need to know the rules: what goes in
> `pg_trickle_core` (pure logic, no pgrx) vs the extension crate (SPI, FFI,
> SQL functions). Add a section to `CONTRIBUTING.md` explaining the crate
> boundary, the `DatabaseBackend` trait contract, and how to add a new
> operator to the core crate.

Verify: CONTRIBUTING.md updated with crate boundary rules.
Dependencies: PGL-1-5. Schema change: No.

**UX-3 — ARCHITECTURE.md update for two-crate layout**

> **In plain terms:** The module layout diagram in `docs/ARCHITECTURE.md`
> and `AGENTS.md` must reflect the new two-crate structure. Update both
> files so new contributors see the correct layout.

Verify: `docs/ARCHITECTURE.md` and `AGENTS.md` module diagrams show
`pg_trickle_core/` and `pg_trickle/` crates.
Dependencies: PGL-1-7. Schema change: No.

### Test Coverage

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| TEST-1 | Delta SQL snapshot tests for all 22 TPC-H queries | M | P0 |
| TEST-2 | Pure-Rust unit tests for extracted operators | L | P0 |
| TEST-3 | Mock DatabaseBackend for in-memory testing | M | P1 |
| TEST-4 | WASM build smoke test in CI | S | P0 |
| TEST-5 | Cargo deny / audit for new crate | XS | P0 |

**TEST-1 — Delta SQL snapshot tests for all 22 TPC-H queries**

> **In plain terms:** Before extraction, capture the exact delta SQL output
> for each of the 22 TPC-H stream table definitions. After extraction, run
> the same generator and diff. Any change is a hard failure. This is the
> primary correctness gate for the refactoring.

Verify: `cargo test -p pg_trickle_core -- snapshot` passes with zero diffs.
Dependencies: CORR-1. Schema change: No.

**TEST-2 — Pure-Rust unit tests for extracted operators**

> **In plain terms:** The 23 operator files currently have ~1,700 unit tests
> that run inside `cargo pgrx test` (requires PostgreSQL). After extraction,
> all pure-logic tests should run via `cargo test -p pg_trickle_core`
> without a database. Tests that require SPI (e.g., catalog lookups) stay
> in the extension crate. Audit and migrate every test that can run without
> PostgreSQL.

Verify: > 80% of existing operator unit tests run in `pg_trickle_core`
without PostgreSQL.
Dependencies: PGL-1-2, TEST-3. Schema change: No.

**TEST-3 — Mock DatabaseBackend for in-memory testing**

> **In plain terms:** For core crate tests that need to call the parser or
> SPI, provide a `MockBackend` that returns canned parse trees and query
> results. This allows testing the full pipeline (parse -> rewrite ->
> operator tree -> delta SQL) without PostgreSQL.

Verify: `MockBackend` supports at least: `raw_parser()` returning a canned
`OpTree`, and `spi_query()` returning a canned result set. 10+ tests use it.
Dependencies: PGL-1-5. Schema change: No.

**TEST-4 — WASM build smoke test in CI**

> **In plain terms:** Add a CI job that compiles `pg_trickle_core` to
> `wasm32-unknown-emscripten` on every PR. This catches platform-specific
> code leaks before they accumulate. The job does not need to run the WASM
> binary — just compile it.

Verify: CI job `build-wasm` passes on every PR targeting the core crate.
Dependencies: PGL-1-6, STAB-5. Schema change: No.

**TEST-5 — Cargo deny / audit for new crate**

> **In plain terms:** The new `pg_trickle_core` crate may introduce new
> transitive dependencies. Ensure `cargo deny check` and `cargo audit`
> cover the new crate and report no advisories.

Verify: `cargo deny check` and `cargo audit` pass for the full workspace.
Dependencies: PGL-1-1. Schema change: No.

### Conflicts & Risks

1. **STAB-1 is the critical path.** The ~500 unsafe blocks in `rewrites.rs`
   and `sublinks.rs` require a `NodeVisitor` abstraction over raw
   `pg_sys` pointer traversal. This is the highest-effort, highest-risk
   item. If the abstraction proves too leaky (e.g., too many pg_sys node
   types to wrap), consider leaving `rewrites.rs` and `sublinks.rs` in the
   extension crate and extracting only operators + DAG + types to the core
   crate. This reduces v0.20.0 scope but still delivers the WASM-compilable
   operator engine for v0.21.0.

2. **PERF-1 must be validated before merging.** Introducing a
   `trait DatabaseBackend` could add vtable dispatch overhead on the hot
   refresh path. Use monomorphization (generics) rather than `dyn Trait`
   for the extension-side implementation. If Criterion shows > 1%
   regression, investigate `#[inline]` annotations and LTO settings.

3. **No schema changes, but workspace restructuring can break `cargo pgrx`.**
   The `cargo-pgrx` tool makes assumptions about workspace layout (e.g.,
   expecting a single `lib.rs` entry point). Test `cargo pgrx package`,
   `cargo pgrx test`, and `cargo pgrx run` early. If `cargo-pgrx` 0.17.x
   cannot handle the workspace, consider upgrading to a newer pgrx that
   supports workspaces, or use a `[patch]` section in `Cargo.toml`.

4. **TEST-2 depends on TEST-3 (MockBackend).** Pure-Rust operator tests
   need a way to feed canned parse trees. Build the MockBackend early so
   TEST-2 can proceed.

5. **WASM target may not be available in standard CI runners.** The
   `wasm32-unknown-emscripten` target requires Emscripten SDK. Either
   install it in CI (adds ~2 min setup) or use a pre-built Docker image
   with the SDK. Budget for CI setup time.

6. **Extraction is all-or-nothing per module.** Partially extracting a
   module (e.g., moving half of `rewrites.rs`) creates circular
   dependencies. Each module must move completely or stay. Plan the
   extraction order: types -> operators -> DAG -> diff -> rewrites ->
   sublinks.

> **v0.20.0 total: ~3–4 weeks (extraction) + ~1–2 weeks (abstraction layer + testing)**

**Exit criteria:**
- [ ] PGL-1-1: `pg_trickle_core` crate exists as a workspace member with zero pgrx dependencies
- [ ] PGL-1-2: All operator delta SQL generation lives in the core crate
- [ ] PGL-1-3: All auto-rewrite passes live in the core crate
- [ ] PGL-1-4: DAG computation lives in the core crate
- [ ] PGL-1-5: `trait DatabaseBackend` defined; pgrx implementation passes all existing tests
- [ ] PGL-1-6: `cargo build --target wasm32-unknown-emscripten -p pg_trickle_core` succeeds
- [ ] PGL-1-7: `just test-all` passes with zero regressions
- [ ] CORR-1: Delta SQL snapshot tests pass for all 22 TPC-H queries (byte-for-byte match)
- [ ] CORR-2: OpTree serialize-deserialize round-trip passes proptest
- [ ] CORR-3: Rewrite pass ordering golden snapshot matches
- [ ] CORR-4: DAG cycle detection passes with 3 new edge-case tests
- [ ] STAB-1: Zero `pg_sys::` references in `pg_trickle_core/src/`
- [ ] STAB-2: `cargo build -p pg_trickle_core --no-default-features` passes in CI
- [ ] STAB-3: `cargo pgrx package` and `cargo pgrx test` succeed with workspace layout
- [ ] STAB-4: Extension upgrade path tested (`0.19.0 -> 0.20.0`)
- [ ] STAB-5: WASM target builds in CI
- [ ] PERF-1: Criterion shows < 1% regression on `diff_operators` benchmark
- [ ] PERF-2: Full benchmark suite passes with < 5% regression threshold
- [ ] TEST-1: TPC-H delta SQL snapshot tests pass
- [ ] TEST-2: > 80% of operator unit tests run without PostgreSQL
- [ ] TEST-3: MockBackend used by 10+ core crate tests
- [ ] TEST-4: CI `build-wasm` job passes on every PR
- [ ] TEST-5: `cargo deny check` and `cargo audit` pass for workspace
- [ ] UX-1: All existing `just` targets pass; `just test-core` added
- [ ] UX-3: ARCHITECTURE.md and AGENTS.md updated with two-crate layout
- [ ] `just check-version-sync` passes

---

## v0.21.0 — PGlite WASM Extension

> **Release Theme**
> This release delivers the first working PGlite extension — the moment
> pg_trickle's incremental view maintenance runs in the browser. By
> wrapping `pg_trickle_core` (extracted in v0.20.0) in a thin C/FFI shim
> and compiling to WASM via PGlite's Emscripten toolchain, we ship an npm
> package (`@pgtrickle/pglite`) that gives PGlite users the full DVM
> operator vocabulary — outer joins, window functions, subqueries,
> recursive CTEs — in IMMEDIATE mode. This dramatically exceeds pg_ivm's
> PGlite offering (INNER joins + basic aggregates only). The release also
> establishes the cross-platform correctness and performance baselines that
> all future PGlite work builds on.

See [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A and §7
Phase 2 for the full architecture.

### PGlite WASM Build (Phase 2)

> **In plain terms:** This takes the `pg_trickle_core` crate extracted in
> v0.20.0 and wraps it in a thin C shim that PGlite's Emscripten-based
> extension build system can compile to WASM. The result is a PGlite
> extension package (`@pgtrickle/pglite`) that provides
> `create_stream_table()`, `drop_stream_table()`, and `alter_stream_table()`
> — all running IMMEDIATE mode inside the WASM PostgreSQL engine with the
> full DVM operator set.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PGL-2-1 | **C shim for PGlite.** Thin C wrapper bridging PGlite's Emscripten environment to `pg_trickle_core` via Rust FFI. Handles `raw_parser` calls through PGlite's built-in PostgreSQL parser. | 1–2wk | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-2-2 | **`DatabaseBackend` for PGlite.** Implement the trait for PGlite's single-connection SPI and built-in parser. Remove advisory lock acquisition (trivial in single-connection). | 3–5d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §5 Strategy A |
| PGL-2-3 | **WASM bundle build.** Integrate with PGlite's extension toolchain (`postgres-pglite`). Produce `.tar.gz` WASM bundle. Target bundle size < 2 MB. | 3–5d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §8 |
| PGL-2-4 | **TypeScript wrapper.** `@pgtrickle/pglite` npm package with PGlite plugin API. `createStreamTable()`, `dropStreamTable()`, `alterStreamTable()` with full IMMEDIATE mode support. | 2–3d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §7 Phase 2 |
| PGL-2-5 | **IMMEDIATE mode E2E tests on PGlite.** Verify inner joins, outer joins, aggregates, DISTINCT, UNION ALL, window functions, subqueries, CTEs (non-recursive + recursive), LATERAL, view inlining, DISTINCT ON, GROUPING SETS. | 1–2wk | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §4.1 |
| PGL-2-6 | **PG 17 vs PG 18 parse tree compatibility.** PGlite tracks PG 17; pg_trickle targets PG 18. Audit and gate any node struct differences with conditional compilation. | 3–5d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §8 |

> **Phase 2 subtotal: ~5–7 weeks**

### Correctness

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | PG 17/18 parse tree node divergence audit | M | P0 |
| CORR-2 | Delta SQL cross-platform equivalence | M | P0 |
| CORR-3 | Advisory lock no-op safety proof | S | P1 |
| CORR-4 | IMMEDIATE trigger ordering in single-connection | S | P1 |

**CORR-1 — PG 17/18 parse tree node divergence audit**

> **In plain terms:** PGlite embeds PostgreSQL 17's parser; pg_trickle's
> `OpTree` construction targets PostgreSQL 18 node structs. Any struct
> layout difference (added fields, renamed members, changed enum values)
> would cause the C shim to misinterpret parse trees, producing silently
> wrong delta SQL. Systematically diff the PG 17 and PG 18 parse tree
> headers (`nodes/parsenodes.h`, `nodes/primnodes.h`) and catalog every
> node type that pg_trickle traverses. Gate incompatible nodes behind
> `#[cfg(pg17)]` / `#[cfg(pg18)]` conditional compilation.

Verify: a CI job compiles `pg_trickle_core` against both PG 17 and PG 18
parse tree headers. A test generates OpTrees from the same SQL on both
versions and asserts structural equality.
Dependencies: PGL-2-6. Schema change: No.

**CORR-2 — Delta SQL cross-platform equivalence**

> **In plain terms:** The same SQL view definition must produce the exact
> same delta SQL on native PostgreSQL 18 and PGlite (WASM + PG 17 parser).
> Any divergence means one platform gets wrong incremental results. Create
> a snapshot test suite that runs all 22 TPC-H stream table definitions
> through both the native and WASM `DatabaseBackend` implementations and
> asserts byte-for-byte identical delta SQL output.

Verify: snapshot comparison test passes for all 22 TPC-H queries on both
platforms. Any diff is a hard failure.
Dependencies: PGL-2-2, CORR-1. Schema change: No.

**CORR-3 — Advisory lock no-op safety proof**

> **In plain terms:** The native extension uses `pg_advisory_xact_lock()`
> to prevent concurrent refresh of the same stream table. PGlite is
> single-connection — the lock acquisition is a no-op. Verify that
> removing the lock cannot cause re-entrancy (a trigger firing
> `create_stream_table()` from within a refresh) by auditing all SPI
> call paths from the PGlite `DatabaseBackend` for re-entrant calls.

Verify: code review + integration test that attempts re-entrant refresh
from within a trigger. Must error cleanly, not corrupt state.
Dependencies: PGL-2-2. Schema change: No.

**CORR-4 — IMMEDIATE trigger ordering in single-connection**

> **In plain terms:** IMMEDIATE mode relies on AFTER triggers firing in a
> specific order when multiple source tables are modified in the same
> statement (e.g., a CTE with multiple INSERTs). Verify that PGlite's
> trigger execution order matches native PostgreSQL's for the trigger
> configurations pg_trickle creates.

Verify: integration test with multi-table CTE INSERT on PGlite; assert
stream table state matches native.
Dependencies: PGL-2-5. Schema change: No.

### Stability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| STAB-1 | WASM heap OOM graceful degradation | M | P0 |
| STAB-2 | C shim panic/unwind boundary safety | S | P0 |
| STAB-3 | Extension load/unload lifecycle correctness | S | P0 |
| STAB-4 | Native extension upgrade path (0.20 → 0.21) | S | P0 |
| STAB-5 | npm package version synchronization | XS | P1 |

**STAB-1 — WASM heap OOM graceful degradation**

> **In plain terms:** WASM environments have a finite heap (typically
> 256 MB in browsers, configurable in Node). A large stream table with
> many operators could exhaust WASM memory during OpTree construction or
> delta SQL generation. The extension must detect allocation failures and
> return a clear PostgreSQL error rather than crashing the WASM instance
> (which would kill all PGlite state). Implement a memory-aware allocator
> wrapper or check `emscripten_get_heap_size()` at entry points.

Verify: stress test creating stream tables over increasingly complex views
until OOM; assert PGlite remains functional and returns an actionable error.
Dependencies: PGL-2-1. Schema change: No.

**STAB-2 — C shim panic/unwind boundary safety**

> **In plain terms:** Rust panics must not cross the FFI boundary into C.
> The C shim must catch panics via `std::panic::catch_unwind()` and
> convert them to PostgreSQL `ereport(ERROR)` calls. Any uncaught panic in
> WASM would abort the entire PGlite instance. Audit every `#[no_mangle]
> extern "C"` entry point in the shim for panic safety.

Verify: test that triggers a panic path (e.g., invalid SQL) from TypeScript;
assert PGlite returns a SQL error, not a WASM trap.
Dependencies: PGL-2-1. Schema change: No.

**STAB-3 — Extension load/unload lifecycle correctness**

> **In plain terms:** PGlite extensions can be loaded and unloaded. The
> C shim must free all Rust-allocated memory on unload and not leave
> dangling pointers or leaked state. Test the full lifecycle: load
> extension → create stream tables → drop stream tables → unload
> extension → reload extension → create new stream tables.

Verify: lifecycle test with memory profiling shows zero leaked allocations
after unload/reload cycle.
Dependencies: PGL-2-1, PGL-2-4. Schema change: No.

**STAB-4 — Native extension upgrade path (0.20 → 0.21)**

> **In plain terms:** v0.21.0 adds PGlite support but makes no SQL-visible
> changes to the native extension. The upgrade migration from 0.20.0 to
> 0.21.0 must leave existing stream tables intact and refreshable.

Verify: upgrade E2E test confirms stream tables survive and refresh
correctly after `0.20.0 -> 0.21.0`.
Dependencies: None. Schema change: No.

**STAB-5 — npm package version synchronization**

> **In plain terms:** The `@pgtrickle/pglite` npm package version must
> match the extension version (0.21.0). Add a CI check that verifies
> `package.json` version matches `pg_trickle.control` version, similar to
> the existing `just check-version-sync` target.

Verify: `just check-version-sync` also validates npm package version.
Dependencies: PGL-2-4. Schema change: No.

### Performance

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| PERF-1 | WASM vs native refresh latency benchmark | M | P0 |
| PERF-2 | WASM bundle size optimization (< 2 MB target) | M | P0 |
| PERF-3 | PGlite cold-start extension load time | S | P1 |

**PERF-1 — WASM vs native refresh latency benchmark**

> **In plain terms:** WASM is expected to be 1.5–3× slower than native
> (per PLAN_PGLITE.md §8). Quantify the actual overhead by benchmarking
> IMMEDIATE-mode refresh on both platforms using the same schema + data.
> The overhead must stay below the threshold where IMMEDIATE mode is still
> faster than full re-evaluation — otherwise PGlite users would be better
> off just re-running the query. Establish a Criterion-like benchmark suite
> for PGlite (potentially using Node.js + `@electric-sql/pglite`).

Verify: benchmark report showing WASM refresh latency for 5 representative
stream tables (scan, join, aggregate, window, recursive CTE). Document
native-to-WASM overhead ratio.
Dependencies: PGL-2-5. Schema change: No.

**PERF-2 — WASM bundle size optimization (< 2 MB target)**

> **In plain terms:** The WASM bundle must be < 2 MB for acceptable
> download times in browser environments (PostGIS is 8.2 MB, pgcrypto is
> 1.1 MB — pg_trickle should be closer to pgcrypto). Apply `wasm-opt -Oz`,
> LTO, `codegen-units = 1`, strip debug info, and feature-gate large
> operator modules (e.g., recursive CTE, window functions) behind optional
> features if needed to meet the target.

Verify: CI job measures WASM bundle size after `wasm-opt` and fails if > 2
MB. Document size breakdown by operator module.
Dependencies: PGL-2-3. Schema change: No.

**PERF-3 — PGlite cold-start extension load time**

> **In plain terms:** The first `CREATE EXTENSION pg_trickle` in a PGlite
> session compiles and loads the WASM module. This must complete in < 500 ms
> in a browser and < 200 ms in Node.js. Measure and optimize by using
> streaming WASM compilation (`WebAssembly.compileStreaming()`) and ensuring
> the extension `_PG_init()` function does minimal work.

Verify: benchmark measuring time from `CREATE EXTENSION` to first
`create_stream_table()` on fresh PGlite instance. Document cold-start time.
Dependencies: PGL-2-1, PGL-2-3. Schema change: No.

### Scalability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| SCAL-1 | Stream table count ceiling in WASM | S | P1 |
| SCAL-2 | Wide-table OpTree memory footprint | S | P1 |
| SCAL-3 | Dataset size practical limit for IMMEDIATE mode | S | P2 |

**SCAL-1 — Stream table count ceiling in WASM**

> **In plain terms:** Each stream table consumes memory for its OpTree,
> delta SQL templates, and trigger metadata. In native PostgreSQL with
> gigabytes of RAM this is trivial, but in a 256 MB WASM heap it matters.
> Determine the practical limit by creating stream tables in a loop until
> OOM, then document the ceiling and add a guard that errors at 80%
> capacity with an actionable message.

Verify: stress test documents the ceiling (e.g., "~200 stream tables with
average 3-table join in 256 MB heap"). Guard errors at 80%.
Dependencies: STAB-1. Schema change: No.

**SCAL-2 — Wide-table OpTree memory footprint**

> **In plain terms:** A stream table over a 100-column source table
> produces a large OpTree and long delta SQL strings. Profile the memory
> consumption of OpTree construction for wide tables and ensure it fits
> within the WASM heap budget alongside typical stream table counts.

Verify: profile OpTree allocation for 10, 50, 100-column source tables.
Document memory per stream table as a function of column count.
Dependencies: PGL-2-5. Schema change: No.

**SCAL-3 — Dataset size practical limit for IMMEDIATE mode**

> **In plain terms:** IMMEDIATE mode fires triggers on every DML, so
> overhead scales with write frequency. In a WASM environment with ~2×
> slower execution, determine at what dataset size (rows × columns ×
> writes/second) IMMEDIATE mode becomes impractical. Document the
> breakpoint so PGlite users know when their use case has outgrown the
> browser and should migrate to native pg_trickle with DIFFERENTIAL mode.

Verify: benchmark with increasing write rates; document the throughput
ceiling (e.g., "> 10K rows/sec INSERT rate degrades stream table latency
past 100 ms").
Dependencies: PERF-1. Schema change: No.

### Ease of Use

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| UX-1 | TypeScript API ergonomics and type safety | S | P0 |
| UX-2 | PGlite getting-started guide | M | P0 |
| UX-3 | WASM-context error message quality | S | P1 |
| UX-4 | npm package README with runnable examples | S | P1 |

**UX-1 — TypeScript API ergonomics and type safety**

> **In plain terms:** The `@pgtrickle/pglite` TypeScript API must follow
> PGlite plugin conventions (`PGlitePlugin` interface, `init()` lifecycle).
> All methods must be fully typed — no `any` types. The API surface must
> be minimal: `createStreamTable(sql)`, `dropStreamTable(name)`,
> `alterStreamTable(name, sql)`, `listStreamTables()`, and
> `refreshStreamTable(name)`. Review against existing PGlite plugins
> (`@electric-sql/pglite-repl`, `pglite-vector`) for consistency.

Verify: TypeScript strict mode compilation with no errors. API review
against PGlite plugin conventions checklist.
Dependencies: PGL-2-4. Schema change: No.

**UX-2 — PGlite getting-started guide**

> **In plain terms:** A `docs/tutorials/PGLITE_QUICKSTART.md` guide
> walking a user from `npm install` to a working React app with live
> stream tables in < 10 minutes. Include: install, create PGlite instance
> with extension, define source table + stream table, insert data, observe
> stream table update. Provide a CodeSandbox / StackBlitz link for
> zero-install try-it-now experience.

Verify: a new developer can follow the guide and see a working stream table
in PGlite in a browser within 10 minutes.
Dependencies: PGL-2-4, UX-1. Schema change: No.

**UX-3 — WASM-context error message quality**

> **In plain terms:** Error messages from the Rust/C shim must be
> JavaScript-friendly: no raw pg_sys error codes, no memory addresses.
> Every error must include the stream table name, the failing SQL
> fragment, and a remediation hint. Unsupported features (DIFFERENTIAL
> mode, scheduled refresh, parallel workers) must error with
> "Not supported in PGlite: <feature>. Use IMMEDIATE mode." rather than
> cryptic internal errors.

Verify: audit all error paths in the C shim + PGlite `DatabaseBackend`.
Every error message includes table name + remediation hint.
Dependencies: PGL-2-1, PGL-2-2. Schema change: No.

**UX-4 — npm package README with runnable examples**

> **In plain terms:** The npm package must have a README with: badge for
> PGlite compatibility, install command, 3 runnable examples (basic
> aggregate, join, window function), API reference, link to the full
> PGlite quickstart guide, and a "Limitations vs native pg_trickle"
> section clearly stating: no DIFFERENTIAL mode, no scheduled refresh,
> no parallel workers, PG 17 parser only.

Verify: README renders correctly on npmjs.com; examples are copy-pasteable
into a Node.js REPL.
Dependencies: PGL-2-4, UX-2. Schema change: No.

### Test Coverage

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| TEST-1 | Full DVM operator E2E suite on PGlite | L | P0 |
| TEST-2 | PG 17/18 parse tree compatibility tests | M | P0 |
| TEST-3 | WASM memory stress tests | M | P1 |
| TEST-4 | TypeScript integration tests | M | P0 |
| TEST-5 | Bundle size regression gate in CI | S | P0 |

**TEST-1 — Full DVM operator E2E suite on PGlite**

> **In plain terms:** Run every DVM operator (23 operators across inner
> join, outer join, full join, semi-join, anti-join, aggregate, distinct,
> union/intersect/except, subquery, scalar subquery, CTE scan, recursive
> CTE, lateral function, lateral subquery, window function, scan, filter,
> project) through IMMEDIATE mode in PGlite. This is the primary
> correctness gate for the WASM extension. Use a Node.js test harness
> with `@electric-sql/pglite` to run the tests headlessly.

Verify: test suite with ≥ 1 test per operator (23+ tests) passes in CI
using PGlite Node.js. Test matrix: INSERT, UPDATE, DELETE for each operator.
Dependencies: PGL-2-5. Schema change: No.

**TEST-2 — PG 17/18 parse tree compatibility tests**

> **In plain terms:** For every parse tree node type that pg_trickle
> traverses, generate a test query that exercises that node, parse it on
> both PG 17 (PGlite) and PG 18 (native), and assert that the resulting
> `OpTree` is structurally identical. This catches version-specific
> divergences before they reach users.

Verify: compatibility test suite covers all node types referenced in
`pg_trickle_core`. Any divergence is a hard failure with clear diagnostic.
Dependencies: CORR-1. Schema change: No.

**TEST-3 — WASM memory stress tests**

> **In plain terms:** Create increasing numbers of stream tables with
> increasing complexity until OOM. Verify that: (a) the guard from SCAL-1
> fires at 80% capacity, (b) PGlite remains functional after the guard
> fires, (c) dropping stream tables actually frees memory. Run under
> different heap sizes (64 MB, 128 MB, 256 MB) to validate the guard
> thresholds.

Verify: stress test with 3 heap sizes completes without WASM trap. Guard
fires at documented threshold. Memory reclaimed after DROP.
Dependencies: STAB-1, SCAL-1. Schema change: No.

**TEST-4 — TypeScript integration tests**

> **In plain terms:** Test the `@pgtrickle/pglite` TypeScript API end-to-end
> using Jest or Vitest in Node.js. Cover: create/drop/alter stream table,
> error handling (invalid SQL, unsupported features), plugin lifecycle
> (init/cleanup), and concurrent operations on different stream tables.
> Run as part of CI on every PR that touches `pg_trickle_pglite/`.

Verify: ≥ 20 TypeScript integration tests pass in CI. Test coverage report
for the TypeScript wrapper shows > 90% line coverage.
Dependencies: PGL-2-4, UX-1. Schema change: No.

**TEST-5 — Bundle size regression gate in CI**

> **In plain terms:** Add a CI job that builds the WASM bundle, runs
> `wasm-opt`, measures the final `.wasm` file size, and fails if it
> exceeds 2 MB. Store the current size as a baseline and alert on any
> increase > 10%. This prevents bundle bloat as features are added.

Verify: CI job `check-wasm-size` runs on every PR touching
`pg_trickle_core/` or `pg_trickle_pglite/`. Fails at > 2 MB.
Dependencies: PGL-2-3, PERF-2. Schema change: No.

### Conflicts & Risks

1. **CORR-1 (PG 17/18 parse tree compatibility) is the highest risk.**
   PGlite embeds PG 17; pg_trickle targets PG 18. If node struct layouts
   diverged significantly between versions (e.g., `JoinExpr` gained a
   field, `RangeTblEntry` changed a flag), the C shim must handle both
   layouts via conditional compilation. In the worst case, some operators
   may need version-specific code paths. Start this audit early — it
   blocks PGL-2-1 and PGL-2-2.

2. **PERF-2 (bundle size < 2 MB) may conflict with full operator coverage.**
   If the 23-operator delta SQL generator compiles to > 2 MB, we may need
   to feature-gate rarely-used operators (recursive CTE, GROUPING SETS)
   behind cargo features. This would reduce the "full DVM vocabulary" claim
   and require documenting which operators are available by default.
   Measure early with a minimal build to establish baseline.

3. **PGlite's Emscripten toolchain is a moving target.** PGlite's
   extension build system (`postgres-pglite`) is not yet stable. Breaking
   changes in the toolchain could block PGL-2-3. Pin the PGlite version
   and track upstream releases. Have a fallback plan: manual Emscripten
   compilation without the PGlite toolchain.

4. **STAB-2 (panic boundary) and STAB-1 (OOM handling) interact.** A Rust
   OOM in WASM triggers a panic, which must not cross the FFI boundary.
   Both items must be implemented together: the OOM guard (STAB-1) sets a
   pre-panic threshold, and the catch_unwind wrapper (STAB-2) is the
   last-resort safety net.

5. **No prior C FFI in the codebase.** The only C code is `scripts/pg_stub.c`
   (test helper). The C shim (PGL-2-1) introduces a new language and
   toolchain requirement. Ensure the C code is minimal (< 500 lines),
   well-documented, and covered by the TypeScript integration tests.

6. **TEST-1 and TEST-4 require a PGlite-based CI runner.** Need Node.js
   18+ with `@electric-sql/pglite` in CI. This is a new CI dependency.
   Add it to the existing CI matrix as a separate job that only runs when
   `pg_trickle_pglite/` or `pg_trickle_core/` files are modified.

> **v0.21.0 total: ~5–7 weeks (WASM build) + ~2–3 weeks (testing + polish)**

**Exit criteria:**
- [ ] PGL-2-1: C shim compiles and links against PGlite's WASM PostgreSQL headers
- [ ] PGL-2-2: PGlite `DatabaseBackend` passes all IMMEDIATE-mode operator tests
- [ ] PGL-2-3: WASM bundle size < 2 MB after `wasm-opt`
- [ ] PGL-2-4: `@pgtrickle/pglite` npm package published to npmjs.com
- [ ] PGL-2-5: All 23 DVM operators pass E2E tests on PGlite
- [ ] PGL-2-6: PG 17 parse tree differences documented and handled with `#[cfg]`
- [ ] CORR-1: PG 17/18 parse tree audit complete; compatibility tests pass
- [ ] CORR-2: Delta SQL cross-platform snapshot tests pass for all 22 TPC-H queries
- [ ] CORR-3: Re-entrant refresh test passes on PGlite
- [ ] CORR-4: Multi-table CTE trigger ordering matches native
- [ ] STAB-1: OOM stress test: PGlite survives with actionable error
- [ ] STAB-2: Panic from invalid SQL returns SQL error, not WASM trap
- [ ] STAB-3: Load/unload/reload lifecycle test: zero leaked allocations
- [ ] STAB-4: Extension upgrade path tested (`0.20.0 -> 0.21.0`)
- [ ] PERF-1: WASM vs native benchmark report published (≤ 3× overhead)
- [ ] PERF-2: WASM bundle ≤ 2 MB (CI gated)
- [ ] PERF-3: Cold-start load time < 500 ms browser, < 200 ms Node.js
- [ ] TEST-1: ≥ 23 operator E2E tests pass on PGlite in CI
- [ ] TEST-2: Parse tree compatibility tests cover all traversed node types
- [ ] TEST-3: Memory stress tests pass under 64/128/256 MB heap sizes
- [ ] TEST-4: ≥ 20 TypeScript integration tests with > 90% line coverage
- [ ] TEST-5: CI `check-wasm-size` job passes on every PR
- [ ] UX-1: TypeScript strict mode compilation: zero errors
- [ ] UX-2: PGlite getting-started guide published with CodeSandbox link
- [ ] UX-4: npm README renders correctly on npmjs.com
- [ ] `just check-version-sync` passes (incl. npm package version)

---

## v0.22.0 — PGlite Reactive Integration

> **Release Theme**
> This release completes the PGlite story by bridging the gap between
> database-side incremental view maintenance and front-end UI reactivity.
> By connecting stream table deltas to PGlite's `live.changes()` API and
> providing framework-specific hooks (`useStreamTable()` for React and
> Vue), pg_trickle becomes the first IVM engine to offer truly reactive
> UI bindings — where DOM updates are proportional to changed rows, not
> result set size. This is the local-first developer's final mile: from
> `INSERT` to re-render in a single digit millisecond count, with no
> polling, no diffing, and no full query re-execution.

See [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §7 Phase 3 for the
full reactive integration design.

### Reactive Bindings (Phase 3)

> **In plain terms:** Phase 2 gave PGlite users in-engine IVM. This phase
> connects stream table changes to PGlite's `live.changes()` API and
> provides framework-specific hooks — `useStreamTable()` for React,
> `useStreamTable()` for Vue — so UI components automatically re-render
> when the underlying data changes. For local-first apps like collaborative
> editors, dashboards, and offline-capable tools, this is the last mile
> between incremental SQL and reactive UI.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PGL-3-1 | **`live.changes()` bridge.** Emit INSERT/UPDATE/DELETE change events from stream table delta application to PGlite's live query system. Keyed by `__pgt_row_id`. | 3–5d | [PLAN_PGLITE.md](plans/ecosystem/PLAN_PGLITE.md) §7 Phase 3 |
| PGL-3-2 | **React hooks.** `useStreamTable(query)` hook that subscribes to stream table changes and returns reactive state. Handles mount/unmount lifecycle. | 3–5d | — |
| PGL-3-3 | **Vue composable.** `useStreamTable(query)` composable with equivalent functionality. | 2–3d | — |
| PGL-3-4 | **Documentation and examples.** Local-first app patterns: collaborative todo list, real-time dashboard, offline-first inventory tracker. Published as `@pgtrickle/pglite` docs. | 2–3d | — |
| PGL-3-5 | **Performance benchmarks.** End-to-end latency from `INSERT` to React re-render. Compare against `live.incrementalQuery()` for complex queries (3-table join + aggregate). | 1–2d | — |

> **Phase 3 subtotal: ~2–3 weeks**

### Correctness

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| CORR-1 | Change event fidelity vs stream table state | M | P0 |
| CORR-2 | Multi-row DML atomicity in reactive stream | S | P0 |
| CORR-3 | Hook state consistency after rapid mutations | M | P1 |
| CORR-4 | DELETE/re-INSERT identity stability | S | P1 |

**CORR-1 — Change event fidelity vs stream table state**

> **In plain terms:** The `live.changes()` bridge emits INSERT/UPDATE/DELETE
> events derived from the IMMEDIATE mode delta application. If an event is
> missed, duplicated, or misclassified (e.g., an UPDATE emitted as DELETE +
> INSERT), the React/Vue state will diverge from the actual stream table
> contents. For every DML operation on every DVM operator type, assert that
> the sequence of change events, when applied to an empty accumulator,
> produces a set identical to `SELECT * FROM stream_table`.

Verify: integration test replaying 1,000 random DML operations across all
operator types; final accumulator state matches `SELECT *`. Any divergence
is a hard failure.
Dependencies: PGL-3-1. Schema change: No.

**CORR-2 — Multi-row DML atomicity in reactive stream**

> **In plain terms:** A single `INSERT INTO source SELECT ... FROM
> generate_series(1, 100)` inserts 100 rows and triggers IMMEDIATE mode
> delta application. The `live.changes()` bridge must emit all 100 change
> events as a single batch — not trickle them one-by-one — so that React
> performs a single re-render, not 100. If events leak across batch
> boundaries, the UI shows intermediate states that never existed in the
> database.

Verify: test with 100-row INSERT; assert `useStreamTable()` callback fires
exactly once with all 100 rows. Intermediate renders counted via React
profiler must be ≤ 1.
Dependencies: PGL-3-1, PGL-3-2. Schema change: No.

**CORR-3 — Hook state consistency after rapid mutations**

> **In plain terms:** If a user performs INSERT → DELETE → INSERT on the
> same row within 10 ms (e.g., optimistic UI with undo), the hook must
> resolve to the correct final state. Race conditions between the
> `live.changes()` event stream and React's asynchronous render cycle
> could show stale data. The hook must use a monotonic sequence number
> (from the bridge's event stream) to discard stale updates.

Verify: stress test with 50 rapid mutations on the same row at 1 ms
intervals; final hook state matches `SELECT *`. Test on both React 18
(concurrent mode) and React 19.
Dependencies: PGL-3-1, PGL-3-2. Schema change: No.

**CORR-4 — DELETE/re-INSERT identity stability**

> **In plain terms:** When a row is deleted and a new row with the same PK
> is inserted, the `__pgt_row_id` changes but the PK doesn't. The change
> bridge must emit a DELETE for the old `__pgt_row_id` and an INSERT for
> the new one — not an UPDATE — so that React's reconciler correctly
> unmounts and remounts the component (not just re-renders it). Wrong
> identity semantics cause stale closures and event handler leaks.

Verify: test DELETE + INSERT with same PK; verify React component lifecycle
(unmount + mount, not just update). Use React DevTools profiler.
Dependencies: PGL-3-1, PGL-3-2. Schema change: No.

### Stability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| STAB-1 | Memory leak prevention in long-lived hooks | M | P0 |
| STAB-2 | Subscription cleanup on component unmount | S | P0 |
| STAB-3 | Error boundary integration for hook failures | S | P0 |
| STAB-4 | Native extension upgrade path (0.21 → 0.22) | S | P0 |
| STAB-5 | Framework version compatibility matrix | S | P1 |

**STAB-1 — Memory leak prevention in long-lived hooks**

> **In plain terms:** A `useStreamTable()` hook in a long-lived component
> (e.g., a dashboard that runs for hours) accumulates change events via
> the `live.changes()` subscription. If the bridge or hook retains
> references to processed events, memory grows unboundedly. Implement a
> bounded event buffer (configurable, default 1,000 events) that discards
> processed events after they are applied to the hook's state snapshot.
> After the buffer fills, old entries are garbage-collected.

Verify: 4-hour soak test with continuous 1 row/sec mutations. Heap snapshot
at 1h and 4h shows < 10% growth. No detached DOM nodes or leaked closures.
Dependencies: PGL-3-1, PGL-3-2. Schema change: No.

**STAB-2 — Subscription cleanup on component unmount**

> **In plain terms:** When a React component using `useStreamTable()` is
> unmounted (e.g., route change), the `live.changes()` subscription must
> be cancelled immediately. Failing to clean up causes: (a) memory leaks
> from the change listener, (b) "setState on unmounted component" warnings,
> (c) stale event processing after the component is gone. Use
> `useEffect()` cleanup function with an AbortController pattern.

Verify: mount/unmount cycle test (100 cycles); zero console warnings, zero
leaked subscriptions (verified via PGlite connection subscription count).
Dependencies: PGL-3-2. Schema change: No.

**STAB-3 — Error boundary integration for hook failures**

> **In plain terms:** If the `live.changes()` bridge throws (e.g., stream
> table was dropped while the hook is active), the hook must propagate the
> error to React's error boundary / Vue's `onErrorCaptured` — not swallow
> it silently or crash the app. Provide an `onError` callback option and
> a default that throws to the nearest error boundary.

Verify: test dropping a stream table while `useStreamTable()` is active;
assert error boundary catches the error with an actionable message.
Dependencies: PGL-3-2, PGL-3-3. Schema change: No.

**STAB-4 — Native extension upgrade path (0.21 → 0.22)**

> **In plain terms:** v0.22.0 adds reactive bindings at the TypeScript/npm
> layer only. The native PostgreSQL extension and PGlite WASM extension
> must continue to work unchanged. The upgrade migration from 0.21.0 to
> 0.22.0 must leave existing stream tables and the `@pgtrickle/pglite`
> WASM extension intact.

Verify: upgrade E2E test confirms stream tables survive and refresh
correctly after `0.21.0 -> 0.22.0`. TypeScript API backward compatibility
verified.
Dependencies: None. Schema change: No.

**STAB-5 — Framework version compatibility matrix**

> **In plain terms:** Test `useStreamTable()` against: React 18.x, React
> 19.x, Vue 3.4+. Document which framework versions are supported. Future
> consideration: Svelte 5 (runes), SolidJS, Angular signals — document
> these as "community-contributed" integration points, not first-party.

Verify: CI matrix testing React 18, React 19, Vue 3.4. Published
compatibility table in npm README.
Dependencies: PGL-3-2, PGL-3-3. Schema change: No.

### Performance

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| PERF-1 | INSERT-to-render latency benchmark | M | P0 |
| PERF-2 | Batch rendering efficiency (single re-render) | S | P0 |
| PERF-3 | Bridge overhead vs raw `live.changes()` | S | P1 |

**PERF-1 — INSERT-to-render latency benchmark**

> **In plain terms:** Measure the end-to-end latency from `INSERT INTO
> source_table` to the React component's DOM update. The target is
> < 50% of `live.incrementalQuery()` latency for a 3-table join +
> aggregate at 10K rows (per PLAN_PGLITE.md). This is the headline
> metric: if pg_trickle's reactive path is not significantly faster than
> PGlite's built-in incremental query, the value proposition collapses.

Verify: benchmark suite with 5 complexity levels (scan, filter, join,
aggregate, window). Publish results as a comparison table against
`live.incrementalQuery()`. Target: < 50% latency at 10K rows.
Dependencies: PGL-3-1, PGL-3-2, PGL-3-5. Schema change: No.

**PERF-2 — Batch rendering efficiency (single re-render)**

> **In plain terms:** A bulk INSERT (100 rows) must produce exactly one
> React re-render, not 100. The change bridge must batch events emitted
> within the same transaction into a single `live.changes()` notification.
> Use `queueMicrotask()` or `requestAnimationFrame()` batching in the
> TypeScript wrapper to coalesce rapid-fire events.

Verify: React profiler shows ≤ 1 render per bulk DML. Test with 1, 10,
100, 1000-row INSERTs; render count is always 1.
Dependencies: PGL-3-1, PGL-3-2, CORR-2. Schema change: No.

**PERF-3 — Bridge overhead vs raw `live.changes()`**

> **In plain terms:** The change bridge adds a translation layer between
> the IMMEDIATE mode delta application and PGlite's `live.changes()` API.
> Measure the overhead of this translation (serialization, event
> construction, key mapping) and ensure it is < 5% of total refresh
> latency. If overhead is higher, optimize the bridge's change event
> construction (e.g., avoid JSON round-trips, use structured clones).

Verify: micro-benchmark isolating bridge overhead from WASM refresh time.
Document overhead as percentage of total INSERT-to-event latency.
Dependencies: PGL-3-1. Schema change: No.

### Scalability

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| SCAL-1 | Multiple concurrent subscriptions | S | P1 |
| SCAL-2 | Large result set rendering (10K+ rows) | M | P1 |
| SCAL-3 | Multi-tab / SharedWorker isolation | S | P2 |

**SCAL-1 — Multiple concurrent subscriptions**

> **In plain terms:** A dashboard page may render 5-10 `useStreamTable()`
> hooks simultaneously, each watching a different stream table. The bridge
> must not create per-hook subscriptions to `live.changes()` — instead,
> use a single multiplexed subscription that fans out to registered hooks.
> Measure performance with 1, 5, 10, 20 concurrent hooks.

Verify: benchmark with 20 concurrent `useStreamTable()` hooks; latency
degradation < 20% vs single hook. Memory growth linear (not quadratic).
Dependencies: PGL-3-1, PGL-3-2. Schema change: No.

**SCAL-2 — Large result set rendering (10K+ rows)**

> **In plain terms:** A stream table with 10K+ rows produces a large
> initial snapshot when `useStreamTable()` mounts. The hook must support
> virtualized rendering (integrating with libraries like `react-virtual`
> or `tanstack-virtual`) by providing a stable row identity key
> (`__pgt_row_id`) and fine-grained change signals (which rows changed,
> not just "something changed"). Without this, mounting a 10K-row stream
> table would freeze the UI for seconds.

Verify: demo app with 10K-row stream table using `@tanstack/react-virtual`.
Mount time < 200 ms. Single-row INSERT re-renders only the affected row,
not the full list.
Dependencies: PGL-3-2, PGL-3-4. Schema change: No.

**SCAL-3 — Multi-tab / SharedWorker isolation**

> **In plain terms:** In multi-tab apps using PGlite with SharedWorker,
> each tab gets its own `useStreamTable()` hooks but shares a single
> PGlite instance. The bridge must correctly fan out change events to all
> tabs without cross-tab interference or duplicate processing. Document
> the SharedWorker architecture and test with 3 concurrent tabs.

Verify: 3-tab test with shared PGlite instance via SharedWorker. INSERT in
tab 1 causes re-render in all 3 tabs. No duplicate events. No memory leaks
across tabs.
Dependencies: PGL-3-1. Schema change: No.

### Ease of Use

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| UX-1 | Local-first app example: collaborative todo | M | P0 |
| UX-2 | Real-time dashboard example | M | P0 |
| UX-3 | API reference with interactive playground | S | P1 |
| UX-4 | Migration guide from `live.incrementalQuery()` | S | P1 |

**UX-1 — Local-first app example: collaborative todo**

> **In plain terms:** A complete, runnable React app demonstrating
> pg_trickle + PGlite for a collaborative todo list: multiple "users"
> (simulated in separate components) INSERT/UPDATE/DELETE todos, each
> user's view updates reactively via `useStreamTable()`. Published in
> the monorepo under `examples/pglite-todo/` with a CodeSandbox link.
> This is the primary "show, don't tell" marketing asset.

Verify: example app runs in CodeSandbox with zero local setup. README
explains every code section. A non-pg_trickle developer can understand it
in 5 minutes.
Dependencies: PGL-3-2, PGL-3-4. Schema change: No.

**UX-2 — Real-time dashboard example**

> **In plain terms:** A React dashboard with 3 stream tables: (a) live
> order count (aggregate), (b) revenue by region (join + aggregate), (c)
> top products (window function + LIMIT). Data is inserted via a simulated
> event stream. Each panel updates reactively. Demonstrates the breadth of
> SQL operators supported in PGlite, beyond what `live.incrementalQuery()`
> can efficiently handle.

Verify: example app with 3 panels. INSERT 100 orders; all 3 panels update
with a single render each. Published to CodeSandbox.
Dependencies: PGL-3-2, PGL-3-4. Schema change: No.

**UX-3 — API reference with interactive playground**

> **In plain terms:** An interactive documentation page (MDX or Storybook)
> where users can type SQL, create a stream table, insert data, and see
> the `useStreamTable()` hook update live — all in the browser via PGlite.
> This replaces the need for a local install for initial exploration.

Verify: playground page loads in < 3 seconds. Users can create a stream
table and see reactive updates within 30 seconds of page load.
Dependencies: PGL-3-2, UX-1. Schema change: No.

**UX-4 — Migration guide from `live.incrementalQuery()`**

> **In plain terms:** Users already using PGlite's `live.incrementalQuery()`
> need a clear guide showing: (a) when to switch to pg_trickle (complex
> queries, high-throughput writes, large result sets), (b) how to migrate
> step-by-step (replace `live.incrementalQuery(q)` with
> `createStreamTable(q)` + `useStreamTable(name)`), (c) what to expect
> (latency improvement, memory trade-off, SQL surface differences).

Verify: migration guide published in docs. Includes a before/after code
diff and a decision flowchart.
Dependencies: PGL-3-4, PERF-1. Schema change: No.

### Test Coverage

| ID | Title | Effort | Priority |
|----|-------|--------|----------|
| TEST-1 | Change event fidelity suite (all operators) | L | P0 |
| TEST-2 | React hook lifecycle tests | M | P0 |
| TEST-3 | Vue composable lifecycle tests | M | P0 |
| TEST-4 | Cross-framework render count assertions | S | P0 |
| TEST-5 | Long-running soak test for memory leaks | M | P1 |

**TEST-1 — Change event fidelity suite (all operators)**

> **In plain terms:** For each of the 23 DVM operators, test that the
> `live.changes()` bridge emits the correct change events for INSERT,
> UPDATE, and DELETE on the source table. Replay events into an
> accumulator and assert it matches `SELECT * FROM stream_table`. This
> extends v0.21.0 TEST-1 (operator E2E) by adding the reactive layer.

Verify: ≥ 69 tests (23 operators × 3 DML types). Accumulator matches
`SELECT *` for every test case.
Dependencies: PGL-3-1, v0.21.0 TEST-1. Schema change: No.

**TEST-2 — React hook lifecycle tests**

> **In plain terms:** Test the full lifecycle of `useStreamTable()`:
> (a) initial mount returns current stream table state, (b) INSERT on
> source triggers re-render with new data, (c) unmount cancels
> subscription, (d) remount re-subscribes and returns current state,
> (e) rapid mount/unmount (100 cycles) has no leaks. Use React Testing
> Library with `renderHook()`.

Verify: ≥ 15 tests covering mount, update, unmount, remount, error, and
stress scenarios. Zero console warnings in test output.
Dependencies: PGL-3-2. Schema change: No.

**TEST-3 — Vue composable lifecycle tests**

> **In plain terms:** Equivalent of TEST-2 for Vue: mount, update, unmount,
> remount, error handling. Use Vue Test Utils with `mount()` and
> `wrapper.unmount()`. Test with both Options API and Composition API
> usage patterns.

Verify: ≥ 10 tests covering Vue lifecycle. Zero console warnings.
Dependencies: PGL-3-3. Schema change: No.

**TEST-4 — Cross-framework render count assertions**

> **In plain terms:** For each framework (React, Vue), verify that a bulk
> INSERT (100 rows) triggers exactly 1 render, not 100. This is the
> batching correctness test. Use framework-specific profiling APIs (React
> Profiler, Vue DevTools perf hooks) to count renders.

Verify: render count = 1 for 100-row bulk INSERT in both React and Vue.
CI assertion.
Dependencies: PGL-3-2, PGL-3-3, PERF-2. Schema change: No.

**TEST-5 — Long-running soak test for memory leaks**

> **In plain terms:** Run a React app with `useStreamTable()` for 4 hours
> with 1 mutation/second. Take heap snapshots at 0h, 1h, 2h, 4h. Assert
> heap growth < 10%. Check for detached DOM nodes, leaked event listeners,
> and orphaned closures. This validates STAB-1 under real conditions.

Verify: soak test runs in CI (with a 30-min abbreviated version for PR CI).
Full 4-hour version runs in nightly CI. Heap growth < 10%.
Dependencies: STAB-1, PGL-3-2. Schema change: No.

### Conflicts & Risks

1. **`live.changes()` API stability.** PGlite's `live.changes()` is
   relatively new and its event format may change between PGlite releases.
   Pin the PGlite version and add an adapter layer so the bridge can
   accommodate event format changes without rewriting the React/Vue hooks.
   If PGlite deprecates `live.changes()` before v0.22.0 ships, fall back
   to `LISTEN/NOTIFY` with a custom channel.

2. **CORR-2 (batch atomicity) and PERF-2 (single re-render) are coupled.**
   The batching mechanism must ensure correctness (all-or-nothing event
   delivery) AND performance (single render). Using `queueMicrotask()`
   for batching risks splitting a transaction's events across two
   microtasks if the event stream straddles a microtask boundary. Consider
   explicit transaction-boundary markers in the bridge's event protocol.

3. **React concurrent mode complicates CORR-3 (rapid mutations).** React
   18/19 concurrent features (`startTransition`, `useDeferredValue`) may
   delay or re-order state updates from `useStreamTable()`. The hook must
   use `useSyncExternalStore()` (React 18+) to ensure tearing-free reads.
   This is non-negotiable for correctness.

4. **SCAL-2 (large result set rendering) requires external library
   integration.** The `useStreamTable()` hook should not bundle a
   virtualization library — instead, expose stable row keys and
   fine-grained change signals that integrate with `@tanstack/react-virtual`
   or similar. Document the pattern but do not create a hard dependency.

5. **SCAL-3 (SharedWorker) is exploratory.** PGlite's SharedWorker support
   has known limitations (no concurrent transactions). Mark SCAL-3 as P2
   and scope it to documentation + a proof-of-concept, not production-grade
   support.

6. **No native extension changes in v0.22.0.** This release is entirely
   in the TypeScript/npm layer. Any temptation to add native features
   (e.g., `LISTEN/NOTIFY` bridge, WebSocket push) should be deferred to
   post-1.0. Keep the scope tight: reactive bindings + examples + docs.

> **v0.22.0 total: ~2–3 weeks (bridge + hooks) + ~1–2 weeks (examples + testing + polish)**

**Exit criteria:**
- [ ] PGL-3-1: Stream table changes appear in `live.changes()` event stream
- [ ] PGL-3-2: React `useStreamTable()` hook re-renders on stream table changes
- [ ] PGL-3-3: Vue `useStreamTable()` composable re-renders on stream table changes
- [ ] PGL-3-4: At least 2 example apps published with documentation and CodeSandbox links
- [ ] PGL-3-5: End-to-end latency benchmarked and published
- [ ] CORR-1: 1,000-operation replay test: accumulator matches `SELECT *` for all operators
- [ ] CORR-2: 100-row bulk INSERT triggers exactly 1 re-render
- [ ] CORR-3: 50 rapid same-row mutations: final hook state matches `SELECT *`
- [ ] CORR-4: DELETE + re-INSERT with same PK: correct unmount/mount lifecycle
- [ ] STAB-1: 4-hour soak test: heap growth < 10%
- [ ] STAB-2: 100 mount/unmount cycles: zero leaked subscriptions
- [ ] STAB-3: Stream table dropped while hook active: error boundary catches
- [ ] STAB-4: Extension upgrade path tested (`0.21.0 -> 0.22.0`)
- [ ] STAB-5: CI matrix passes for React 18, React 19, Vue 3.4+
- [ ] PERF-1: INSERT-to-render latency < 50% of `live.incrementalQuery()` at 10K rows
- [ ] PERF-2: Render count = 1 for bulk DML (1, 10, 100, 1000 rows)
- [ ] TEST-1: ≥ 69 change event fidelity tests pass (23 operators × 3 DML types)
- [ ] TEST-2: ≥ 15 React hook lifecycle tests pass
- [ ] TEST-3: ≥ 10 Vue composable lifecycle tests pass
- [ ] TEST-4: Cross-framework render count = 1 for bulk DML
- [ ] TEST-5: 30-min abbreviated soak test passes in PR CI
- [ ] UX-1: Collaborative todo example published to CodeSandbox
- [ ] UX-2: Real-time dashboard example published to CodeSandbox
- [ ] UX-4: Migration guide from `live.incrementalQuery()` published
- [ ] `just check-version-sync` passes (incl. npm package version)

---

## v1.0.0 — Stable Release

**Goal:** First officially supported release. Semantic versioning locks in.
API, catalog schema, and GUC names are considered stable. Focus is
distribution — getting pg_trickle onto package registries — and PostgreSQL 19
forward-compatibility.

### PostgreSQL 19 Forward-Compatibility (A3)

> **In plain terms:** When PostgreSQL 19 beta stabilises and pgrx 0.18.x
> ships with PG 19 support, this milestone bumps the pgrx dependency,
> audits every internal `pg_sys::*` API call for breaking changes, adds
> conditional compilation gates, and validates the WAL decoder against any
> pgoutput format changes introduced in PG 19. Moved here from the
> earlier v0.19.0 milestone because PG 19 beta availability is uncertain.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A3-1 | pgrx version bump to 0.18.x (PG 19 support) + `cargo pgrx init --pg19` | 2–4h | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) §2 |
| A3-2 | `pg_sys::*` API audit: heap access, catalog structs, WAL decoder `LogicalDecodingContext` | 8–16h | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) §3 |
| A3-3 | Conditional compilation (`#[cfg(feature = "pg19")]`) for changed APIs | 4–8h | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) §4 |
| A3-4 | CI matrix expansion for PG 19 + full E2E suite run | 4–8h | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |

> **A3 subtotal: ~18–36 hours**

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
| R4 | ~~CNPG operator hardening (K8s 1.33+ native ImageVolume)~~ ➡️ Pulled to v0.15.0 | 4–6h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |
| R5 | **Docker Hub official image.** Publish `pgtrickle/pg_trickle:1.0.0-pg18` and `:latest` to Docker Hub. Sync Dockerfile.hub version tag with release. Automate via GitHub Actions release workflow. | 2–4h | — |
| R6 | **Version sync automation.** Ensure `just check-version-sync` covers all version references (Cargo.toml, extension control files, Dockerfile.hub, dbt_project.yml, CNPG manifests). Add to CI as a blocking check. | 2–3h | — |
| SAST-SEMGREP | **Elevate Semgrep to blocking in CI.** CodeQL and cargo-deny already block; Semgrep is advisory-only. Flip to blocking for consistent safety gating. Before flipping, verify zero findings across all current rules. | 1–2h | [PLAN_SAST.md](plans/testing/PLAN_SAST.md) |

> **v1.0.0 total: ~36–66 hours** (incl. PG 19 compat ~18–36h + release engineering ~18–30h)

**Exit criteria:**
- [ ] A3: PG 19 builds and passes full E2E suite
- [ ] CI matrix includes PG 19
- [ ] Published on PGXN (stable) and apt/rpm via PGDG
- [ ] Docker Hub image published (`pgtrickle/pg_trickle:1.0.0-pg18` and `:latest`)
- [x] CNPG extension image published to GHCR (`pg_trickle-ext`)
- [x] CNPG cluster-example.yaml validated (Image Volume approach)
- [ ] `just check-version-sync` passes and blocks CI on mismatch
- [ ] SAST-SEMGREP: Semgrep elevated to blocking in CI; zero findings verified
- [ ] Upgrade path from v0.17.0 tested
- [ ] Semantic versioning policy in effect

---

## Post-1.0 — Scale, Ecosystem & Platform Expansion

These are not gated on 1.0 but represent the longer-term horizon. PG backward
compatibility (PG 16–18) and native DDL syntax were moved here from v0.16.0
to keep the pre-1.0 milestones focused on performance and correctness.

### Ecosystem expansion

> **In plain terms:** Building first-class integrations with the tools most
> data teams already use — a proper dbt adapter (beyond just a
> materialization macro), an Airflow provider so you can trigger stream
> table refreshes from Airflow DAGs, a `pgtrickle` TUI for
> managing and monitoring stream tables without writing SQL (shipped in
> v0.14.0), and integration guides for popular ORMs and migration
> frameworks like Django, SQLAlchemy, Flyway, and Liquibase.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| E1 | dbt full adapter (`dbt-pgtrickle` extending `dbt-postgres`) | 20–30h | [PLAN_DBT_ADAPTER.md](plans/dbt/PLAN_DBT_ADAPTER.md) |
| E2 | Airflow provider (`apache-airflow-providers-pgtrickle`) | 16–20h | [PLAN_ECO_SYSTEM.md §4](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| ~~E3~~ | ~~CLI tool (`pgtrickle`) for management outside SQL~~ ➡️ Pulled to v0.14.0 as TUI (E3-TUI) | 4–6d | [PLAN_TUI.md](plans/ui/PLAN_TUI.md) |
| E4 | ~~Flyway / Liquibase migration support~~ ➡️ Pulled to v0.15.0 | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |
| E5 | ~~ORM integrations guide (SQLAlchemy, Django, etc.)~~ ➡️ Pulled to v0.15.0 | 8–12h | [PLAN_ECO_SYSTEM.md §5](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

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

### PG Backward Compatibility (PG 16–18)

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

### Native DDL Syntax

> **In plain terms:** Currently you create stream tables by calling a
> function: `SELECT pgtrickle.create_stream_table(...)`. This adds support
> for standard PostgreSQL DDL syntax: `CREATE MATERIALIZED VIEW my_view
> WITH (pgtrickle.stream = true) AS SELECT ...`. That single change means
> `pg_dump` can back them up properly, `\dm` in psql lists them, ORMs can
> introspect them, and migration tools like Flyway treat them like ordinary
> database objects. Stream tables finally look native to PostgreSQL tooling.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| NAT-1 | `ProcessUtility_hook` infrastructure: register in `_PG_init()`, dispatch+passthrough, hook chaining with TimescaleDB/pg_stat_statements | 3–5d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |
| NAT-2 | CREATE/DROP/REFRESH interception: parse `CreateTableAsStmt` reloptions, route to internal impls, IF EXISTS handling, CONCURRENTLY no-op | 8–13d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |
| NAT-3 | E2E tests: CREATE/DROP/REFRESH via DDL syntax, hook chaining, non-pg_trickle matview passthrough | 2–3d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §Tier 2 |

> **Native DDL syntax subtotal: ~13–21 days**

### Advanced SQL

> **In plain terms:** Longer-horizon features requiring significant research
> — backward-compatibility to PG 14/15, partitioned stream table storage,
> and remaining SQL coverage gaps. Several items have been pulled forward
> to v0.16.0 and v0.17.0.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~A2~~ | ~~Transactional IVM Phase 4 remaining (ENR-based transition tables, C-level triggers, prepared stmt reuse)~~ ➡️ Pulled to v0.17.0 | ~36–54h | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |
| ~~A3~~ | ~~PostgreSQL 19 forward-compatibility~~ ➡️ Pulled to v0.16.0 ➡️ Moved to v1.0.0 | ~18–36h | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |
| A4 | PostgreSQL 14–15 backward compatibility | ~40h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) |
| A5 | Partitioned stream table storage (opt-in) | ~60–80h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §4 |
| ~~A6~~ | ~~Buffer table partitioning by LSN range (`pg_trickle.buffer_partitioning` GUC)~~ | ✅ Done | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 4 §3.3 |
| ~~A8~~ | ~~`ROWS FROM()` with multiple SRF functions~~ ➡️ Pulled to v0.17.0 | ~1–2d | [PLAN_TRANSACTIONAL_IVM_PART_2.md](plans/sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) Task 2.3 |

### Parser Modularization & Shared Template Cache (G13-PRF, G14-SHC)

> **In plain terms:** Two large-effort research items identified in the deep gap
> analysis. Parser modularization is a prerequisite for native DDL syntax (BC2);
> shared template caching eliminates per-connection cold-start overhead.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ~~G13-PRF~~ | ~~**Modularize `src/dvm/parser.rs`.**~~ ✅ Done in v0.15.0 | ~3–4wk | [plans/performance/REPORT_OVERALL_STATUS.md §13](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~G14-SHC~~ | ~~**Shared-memory template caching (research spike).**~~ ➡️ Pulled to v0.16.0 | ~2–3wk | [plans/performance/REPORT_OVERALL_STATUS.md §14](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Parser modularization: ✅ Done in v0.15.0. Template caching: ➡️ v0.16.0**

### Convenience API Functions (G15-BC, G15-EX)

> **In plain terms:** Two quality-of-life API additions that simplify
> programmatic stream table management, useful for dbt/CI pipelines.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| G15-BC | **`bulk_create(definitions JSONB)`** — create multiple stream tables and their CDC triggers in a single transaction. Useful for dbt/CI pipelines that manage many STs programmatically. ➡️ Pulled to v0.15.0 | ~2–3d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |
| ~~G15-EX~~ | ~~**`export_definition(name TEXT)`** — export a stream table configuration as reproducible `CREATE STREAM TABLE … WITH (…)` DDL.~~ ➡️ Pulled to v0.14.0 | ~1–2d | [plans/performance/REPORT_OVERALL_STATUS.md §15](plans/performance/REPORT_OVERALL_STATUS.md) |

> **Convenience API subtotal: ~2–3 days (G15-EX pulled to v0.14.0; G15-BC pulled to v0.15.0)**

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
| v0.12.0 — Scalability Foundations, Partitioning Enhancements & Correctness | ~18–27 wk + ~6–8 wk scalability + ~5–8 wk partitioning + ~1–3 wk defaults | — | |
| v0.13.0 — Scalability Foundations, Partitioning Enhancements, MERGE Profiling & Multi-Tenant Scheduling | ~15–23 wk | — | |
| v0.14.0 — Tiered Scheduling, UNLOGGED Buffers & Diagnostics | ~2–6 wk + ~1 wk patterns + ~2–4d stability + ~3.5–7d diagnostics + ~1–2d export + ~4–6d TUI + ~0.5d docs | — | |
| v0.15.0 — External Test Suites & Integration | ~40–70h + ~2–3d bulk create + ~3–5d planner hints + ~2–3d cache spike + ~3–4wk parser + ~1–2wk watermark + ~2–4wk delta cost/spill | — | ✅ Released |
| v0.16.0 — Performance & Refresh Optimization | ~1–2wk MERGE alts + ~4–6wk aggregate fast-path + ~1–2wk append-only + ~2–3wk predicate pushdown + ~2–3wk template cache + ~2–3wk buffer compaction + ~3–6wk test coverage + ~1–2wk bench CI + ~2–3d auto-indexing + ~12–22h quick wins | — | |
| v0.17.0 — Query Intelligence & Stability | ~2–3wk cost-based strategy + ~3–4wk columnar tracking + ~32–48h TIVM Phase 4 + ~1–2d ROWS FROM + ~2–3wk SQLancer + ~2–3wk incremental DAG + ~4–8h unsafe reduction + ~1–2wk api.rs mod + ~2–3d migration guide + ~3–5d runbook + ~2–3d playground + ~2–3d doc polish | — | |
| v0.18.0 — Hardening & Delta Performance | ~70–100h | — | |
| v0.19.0 — PGlite Proof of Concept | ~2–3wk (plugin) + ~1–2d (version bump) | — | |
| v0.20.0 — Core Extraction (`pg_trickle_core`) | ~3–4wk (extraction) + ~1–2wk (abstraction + testing) | — | |
| v0.21.0 — PGlite WASM Extension | ~5–7wk (WASM build) + ~2–3wk (testing + polish) | — | |
| v0.22.0 — PGlite Reactive Integration | ~2–3wk (bridge + hooks) + ~1–2wk (examples + testing + polish) | — | |
| v1.0.0 — Stable release (incl. PG 19 compat) | ~36–66h | — | |
| Post-1.0 (PG compat + Native DDL) | ~38–56h (PG 16–18) + ~13–21d (Native DDL) | — | |
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
