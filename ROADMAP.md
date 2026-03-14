# pg_trickle — Project Roadmap

> **Last updated:** 2026-03-14
> **Latest release:** 0.5.0 (2026-03-13)
> **Current milestone:** v0.6.0 — Partitioning, Idempotent DDL, Edge Cases & Circular Dependency Foundation

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
 ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
 │ 0.1.x  │ │ 0.2.0  │ │ 0.2.1  │ │ 0.2.2  │ │ 0.2.3  │ │ 0.3.0  │ │ 0.4.0  │ │ 0.5.0  │ │ 0.6.0  │
 │Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Released│─│Partn., │
 │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │DDL&EC  │
 └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘
      │
      └─ ┌────────┐ ┌────────┐ ┌────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐
         │ 0.7.0  │ │ 0.8.0  │ │ 0.9.0  │ │ 0.10.0  │ │ 0.11.0  │ │ 0.12.0  │
         │WM &   │─│Pooler  │─│Observ.,│─│Incr.Agg │─│Partn.   │─│Delta,   │
         │Cycl.  │ │Compat. │ │Fuse&Dmp│ │IVM      │ │&Scale   │ │CDC&PGBk │
         └────────┘ └────────┘ └────────┘ └─────────┘ └─────────┘ └─────────┘
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

### Idempotent DDL (`create_or_replace`)

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
| COR-1 | **The core function.** `create_or_replace_stream_table()` compares the new definition against the existing one and picks the cheapest path: no-op if identical, settings-only update if just config changed, column migration if columns were added/dropped, or full rebuild if the query is fundamentally different. One function call replaces the drop-and-recreate dance. | 4h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| COR-3 | **dbt just works.** Updates the `stream_table` dbt materialization macro to call `create_or_replace` instead of dropping and recreating on every `dbt run`. Existing data survives deployments; only genuinely changed stream tables get rebuilt. | 2h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| COR-4 | **Upgrade path and documentation.** Upgrade SQL script so existing installations get the new function via `ALTER EXTENSION UPDATE`. SQL Reference and FAQ updated with usage examples. | 2.5h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |
| COR-5 | **Thorough test coverage.** 13 end-to-end tests covering: identical no-op, config-only change, query change with compatible columns, query change with incompatible columns, mode switches, and error cases. | 4h | [PLAN_CREATE_OR_REPLACE.md](plans/sql/PLAN_CREATE_OR_REPLACE.md) |

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
| EC-19 | **Prevent silent data corruption with WAL + keyless tables.** If you use WAL-based change tracking on a table without a primary key, PostgreSQL needs `REPLICA IDENTITY FULL` to send complete row data. Without it, deltas are silently incomplete. This rejects the combination at creation time with a clear error instead of producing wrong results. | 0.5 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-19 |

#### P1 — Operational Safety

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC-16 | **Detect when someone silently changes a function your query uses.** If a stream table's query calls `calculate_discount()` and someone does `CREATE OR REPLACE FUNCTION calculate_discount(...)` with new logic, the stream table's cached computation plan becomes stale. This checks function body hashes on each refresh and triggers a rebuild when a change is detected. | 2 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-16 |
| EC-18 | **Explain why WAL mode isn't activating.** When `cdc_mode = 'auto'`, pg_trickle is supposed to upgrade from trigger-based to WAL-based change tracking when possible. If it stays stuck on triggers (e.g. because `wal_level` isn't set to `logical`), there's no feedback. This adds a periodic log message explaining the reason and surfaces it in the `health_check()` output. | 1 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-18 |
| EC-34 | **Recover gracefully after restoring from backup.** When you restore a PostgreSQL server from `pg_basebackup`, replication slots are lost. pg_trickle's WAL decoder would fail trying to read from a slot that no longer exists. This detects the missing slot, automatically falls back to trigger-based tracking, and logs a WARNING so you know what happened. | 1 day | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-34 |

#### P2 — Usability Gaps

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC-03 | **Support window functions inside expressions.** Queries like `CASE WHEN ROW_NUMBER() OVER (...) = 1 THEN 'first' ELSE 'other' END` are currently rejected because the incremental engine can't handle a window function nested inside a CASE. This automatically extracts the window function into a preliminary step and rewrites the outer query to reference the precomputed result — so the query pattern just works. | 3–5 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-03 |
| EC-32 | **Support `ALL (subquery)` comparisons.** Queries like `WHERE price > ALL (SELECT price FROM competitors)` (meaning "greater than every row in the subquery") are currently rejected in incremental mode. This rewrites them into an equivalent form the engine can handle, removing a Known Limitation from the changelog. | 2–3 days | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-32 |

> **Edge case hardening subtotal: ~9.5–13.5 days**

### Ergonomics Follow-Up

> **In plain terms:** Several test gaps and a documentation item were left
> over from the v0.5.0 ergonomics work. These are all small E2E tests that
> confirm existing features actually produce the warnings and errors they're
> supposed to — catching regressions before users hit them. The changelog
> entry documents breaking behavioural changes (the default schedule changed
> from a fixed "every 1 minute" to an auto-calculated interval, and `NULL`
> schedule input is now rejected).

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| ERG-T1 | **Test the smart schedule default.** Verify that passing `'calculated'` as a schedule works (pg_trickle picks an interval based on table size) and that passing `NULL` gives a clear error instead of silently breaking. Catches regressions in the schedule parser. | 4h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ERG-T2 | **Test that removed settings stay removed.** The `diamond_consistency` GUC was removed in v0.4.0. Verify that `SHOW pg_trickle.diamond_consistency` returns an error — not a stale value from a previous installation that confuses users. | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ERG-T3 | **Test the "heads up, this will do a full refresh" warning.** When you change a stream table's query via `alter_stream_table(query => ...)`, it may trigger an expensive full re-scan. Verify the WARNING appears so users aren't surprised by a sudden spike in load. | 3h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ERG-T4 | **Test the WAL configuration warning.** When `cdc_mode = 'auto'` but PostgreSQL's `wal_level` isn't set to `logical`, pg_trickle can't use WAL-based tracking and silently falls back to triggers. Verify the startup WARNING appears so operators know they need to change `wal_level`. | 3h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |
| ERG-T5 | **Document breaking changes in the changelog.** In v0.4.0 the default schedule changed from "every 1 minute" to auto-calculated, and `NULL` schedule input started being rejected. These behavioural changes need explicit CHANGELOG entries so upgrading users aren't caught off guard. | 2h | [PLAN_ERGONOMICS.md](plans/PLAN_ERGONOMICS.md) §Remaining follow-up |

> **Ergonomics follow-up subtotal: ~14 hours**

### Bootstrap Source Gating Follow-Up

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
| BOOT-F1 | **Calling gate twice is safe.** Verify that calling `gate_source('orders')` when `orders` is already gated is a harmless no-op — not an error. Important for ETL scripts that may retry on failure. | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| BOOT-F2 | **Gate → ungate → gate again works correctly.** Verify the full lifecycle: gate a source (scheduler skips it), ungate it (scheduler resumes), gate it again (scheduler skips again). Proves the mechanism is reusable across multiple load cycles. | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| BOOT-F3 | **See your gates at a glance.** A new `bootstrap_gate_status()` function that shows which sources are gated, when they were gated, who gated them, and how long they've been paused. Useful for debugging when the scheduler seems to be "doing nothing" — it might just be waiting for a gate. | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |
| BOOT-F4 | **Cookbook for common ETL patterns.** Documentation with step-by-step recipes: gating a single source during a bulk load, coordinating multiple source loads that must finish together, gating only part of a stream table DAG, and the classic "nightly batch → gate → load → ungate → single clean refresh" workflow. | 3h | [PLAN_BOOTSTRAP_GATING.md](plans/sql/PLAN_BOOTSTRAP_GATING.md) |

> **Bootstrap gating follow-up subtotal: ~12 hours**

### dbt Integration Enhancements

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
| DBT-1 | **Check stream table health from dbt.** A new `stream_table_status()` macro that returns whether a stream table is healthy, stale, or erroring — so you can write dbt tests like "fail if the orders summary hasn't refreshed in the last 5 minutes." Makes pg_trickle a first-class citizen in dbt's testing framework. | 3h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |
| DBT-2 | **Refresh everything in one command.** A `dbt run-operation refresh_all_stream_tables` command that refreshes all stream tables in the correct dependency order. Designed for CI pipelines: run it after `dbt run` and before `dbt test` to make sure all materialized data is current. | 2h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |
| DBT-3 | **Test the dbt ↔ alter flow.** Integration tests that verify query changes, config changes, and mode switches all work correctly when made through dbt's `stream_table` materialization. Especially important now that `create_or_replace` is landing in the same release. | 3h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 1 |

> **dbt integration subtotal: ~8 hours**

### SQL Documentation Gaps

> **In plain terms:** Once EC-03 (window functions in expressions) and EC-32
> (`ALL (subquery)`) are implemented in this release, the documentation needs
> to explain the new patterns with examples. The foreign table polling CDC
> feature (shipped in v0.2.2) also needs a worked example showing common
> setups like `postgres_fdw` source tables with periodic polling.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| DOC-1 | **Show users how ALL-subqueries work.** Once EC-32 lands, add a SQL Reference section explaining `WHERE price > ALL (SELECT ...)`, how pg_trickle rewrites it internally, and a complete worked example with sample data and expected output. | 2h | [GAP_SQL_OVERVIEW.md](plans/sql/GAP_SQL_OVERVIEW.md) |
| DOC-2 | **Show the window-in-expression pattern.** Once EC-03 lands, add a before/after example to the SQL Reference: "Here's your original query with `CASE WHEN ROW_NUMBER() ...`, and here's what pg_trickle does under the hood to make it work incrementally." | 2h | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-03 |
| DOC-3 | **Walkthrough for foreign table sources.** A step-by-step recipe showing how to create a `postgres_fdw` foreign table, use it as a stream table source with polling-based change detection, and what to expect in terms of refresh behaviour. This feature shipped in v0.2.2 but was never properly documented with an example. | 1h | Existing feature (v0.2.2) |

> **SQL documentation subtotal: ~5 hours**

> **v0.6.0 total: ~77–92h**

**Exit criteria:**
- [x] Partitioned source tables E2E-tested; ATTACH PARTITION detected
- [x] WAL mode works with `publish_via_partition_root = true`
- [ ] `create_or_replace_stream_table` deployed; dbt macro updated
- [ ] SCC algorithm in place; monotonicity checker rejects non-monotone cycles
- [ ] WAL + keyless without REPLICA IDENTITY FULL rejected at creation (EC-19)
- [ ] `ALTER FUNCTION` body changes detected via `pg_proc` hash polling (EC-16)
- [ ] Stuck `auto` CDC mode surfaces explanation in logs and health check (EC-18)
- [ ] Missing WAL slot after restore auto-detected with TRIGGER fallback (EC-34)
- [ ] Window functions in expressions supported via CTE extraction (EC-03)
- [ ] `ALL (subquery)` rewritten to `NOT EXISTS (... EXCEPT ...)` (EC-32)
- [ ] Ergonomics E2E tests for calculated schedule, warnings, and removed GUCs pass
- [ ] `gate_source()` idempotency and re-gating tested; `bootstrap_gate_status()` available
- [ ] dbt `stream_table_status()` and `refresh_all_stream_tables` macros shipped
- [ ] SQL Reference updated for EC-03, EC-32, and foreign table polling patterns
- [ ] Extension upgrade path tested (`0.5.0 → 0.6.0`)

---

## v0.7.0 — Watermarks, Circular DAG Execution, Observability & Infrastructure

**Goal:** Add user-injected temporal watermark gating for batch-ETL
coordination, complete the fixpoint scheduler for circular stream table
DAGs, ship ready-made Prometheus/Grafana monitoring, and prepare the
1.0 packaging and deployment infrastructure.

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
| WM-1 | Catalog: `pgt_watermarks` table (`source_relid`, `current_watermark`, `updated_at`, `wal_lsn_at_advance`); `pgt_watermark_groups` table (`group_name`, `sources`, `tolerance`) | 2h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| WM-2 | `advance_watermark(source, watermark)` — monotonicity check, store LSN alongside watermark, lightweight scheduler signal | 2h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| WM-3 | `create_watermark_group(name, sources[], tolerance)` / `drop_watermark_group()` | 2h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| WM-4 | Scheduler pre-check: evaluate watermark alignment predicate; skip + log `SKIP(watermark_misaligned)` if not aligned | 3–4h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| WM-5 | `watermarks()`, `watermark_groups()`, `watermark_status()` introspection functions | 2h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |
| WM-6 | E2E tests: nightly ETL, micro-batch tolerance, multiple pipelines, mixed external+internal sources | 6–8h | [PLAN_WATERMARK_GATING.md](plans/sql/PLAN_WATERMARK_GATING.md) |

> **Watermark gating subtotal: ~17–20 hours**

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
| CYC-5 | Scheduler fixpoint iteration: `iterate_to_fixpoint()`, convergence detection from `(rows_inserted, rows_deleted)`, non-convergence → `ERROR` status | ~8h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 5 |
| CYC-6 | Creation-time validation: allow monotone cycles when `allow_circular=true`; assign `scc_id`; recompute SCCs on `drop_stream_table` | ~3h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 6 |
| CYC-7 | Monitoring: `scc_id` + `last_fixpoint_iterations` in views; `pgtrickle.pgt_scc_status()` function | ~2h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 7 |
| CYC-8 | Documentation + E2E tests (`e2e_circular_tests.rs`): 6 scenarios (monotone cycle, non-monotone reject, convergence, non-convergence→ERROR, drop breaks cycle, `allow_circular=false` default) | ~6h | [PLAN_CIRCULAR_REFERENCES.md](plans/sql/PLAN_CIRCULAR_REFERENCES.md) Part 8 |

> **Circular dependencies subtotal: ~19 hours**

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
| OBS-1 | **Prometheus metrics out of the box.** A YAML config file for the standard `postgres_exporter` that turns pg_trickle's existing SQL views into Prometheus metrics: refresh count, success/failure rates, staleness, rows changed, CDC lag, and alerts. Drop the file in and your existing Prometheus setup starts scraping pg_trickle data. | 4h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 2 |
| OBS-2 | **Get paged when things go wrong.** Pre-built Prometheus alerting rules that fire when a stream table has been stale for over 5 minutes, when 3+ consecutive refreshes fail, when CDC replication lag exceeds 1 GB, or when any CDC source has an active alert. Copy the file into your Prometheus config directory. | 2h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 2 |
| OBS-3 | **See everything at a glance.** A Grafana dashboard with five sections: an overview row (active tables, stale count, error count), refresh performance charts (duration trends, throughput), staleness heatmap, CDC health panel (mode per source, replication lag), and a per-table drill-down you can filter with a dropdown. Import the JSON file into Grafana. | 4h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 3 |
| OBS-4 | **Try it all in one command.** A `docker-compose.yml` that spins up PostgreSQL with pg_trickle, postgres_exporter, Prometheus, and Grafana — pre-wired together. Run `docker compose up`, open `localhost:3000`, and see the dashboard with live data. Great for demos and evaluation. | 2h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §Project 3 |

> **Observability subtotal: ~12 hours**

### Pre-1.0 Infrastructure Prep

> **In plain terms:** Three preparatory tasks that make the eventual 1.0
> release smoother. A draft Docker Hub image workflow (tests the build but
> doesn't publish yet); a PGXN metadata file so the extension can eventually
> be installed with `pgxn install pg_trickle`; and a basic CNPG integration
> test that verifies the extension image loads correctly in a CloudNativePG
> cluster. None of these ship user-facing features — they're CI and
> packaging scaffolding.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| INFRA-1 | **Prove the Docker image builds.** Set up a CI workflow that builds the official Docker Hub image (PostgreSQL 18 + pg_trickle pre-installed), runs a smoke test (create extension, create a stream table, refresh it), but doesn't publish anywhere yet. When 1.0 arrives, publishing is just flipping a switch. | 5h | [PLAN_DOCKER_IMAGE.md](plans/infra/PLAN_DOCKER_IMAGE.md) |
| INFRA-2 | **Prepare for `pgxn install pg_trickle`.** PGXN is PostgreSQL's official extension registry. Drafting the `META.json` metadata file now means publishing to PGXN at 1.0 is a single command — no scrambling to figure out the required fields and version constraints at release time. | 2h | [PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) |
| INFRA-3 | **Verify Kubernetes deployment works.** A CI smoke test that deploys the pg_trickle extension image into a CloudNativePG (CNPG) Kubernetes cluster, creates a stream table, and confirms a refresh cycle completes. Catches packaging and compatibility issues before they reach Kubernetes users. | 4h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |

> **Infrastructure prep subtotal: ~11 hours**

> **v0.7.0 total: ~59–62h**

**Exit criteria:**
- [ ] `advance_watermark` + scheduler gating operational; ETL E2E tests pass
- [ ] Monotone circular DAGs converge to fixpoint; non-convergence surfaces as `ERROR`
- [ ] Prometheus queries + alerting rules + Grafana dashboard shipped
- [ ] Docker Hub image draft workflow passes; PGXN `META.json` drafted
- [ ] CNPG integration smoke test passes in CI
- [ ] Extension upgrade path tested (`0.6.0 → 0.7.0`)

---

## v0.8.0 — Connection Pooler Compatibility

**Goal:** Enable cloud-native PgBouncer transaction-mode deployments. Requires
`shared_preload_libraries`.

### Connection Pooler Compatibility

> **In plain terms:** PgBouncer is the most widely used PostgreSQL connection
> pooler — it sits in front of the database and reuses connections across
> many application threads. In its common "transaction mode" it hands a
> different physical connection to each transaction, which breaks anything
> that assumes the same connection persists between calls (session locks,
> prepared statements). This work replaces all such session-scoped state in
> pg_trickle so it works correctly in cloud deployments — Supabase, Railway,
> Neon, and similar platforms that route through PgBouncer by default.

pg_trickle uses session-level advisory locks and `PREPARE` statements that are
incompatible with PgBouncer transaction-mode pooling. This section replaces all
session-scoped state with transaction-scoped equivalents.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PB1 | Replace `pg_advisory_lock()` with `pg_advisory_xact_lock()` across refresh, CDC, and scheduler coordination | 3–4d | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) G8.4 |
| PB2 | Eliminate `PREPARE __pgt_merge_*` prepared statements (replace with inline or per-transaction SQL) | 3–4d | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) G8.4 |
| PB3 | E2E validation against PgBouncer transaction-mode (Docker Compose with pooler sidecar) | 1–2d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-28 |

> **PgBouncer compatibility subtotal: ~7–10 days**

> **v0.8.0 total: ~56–80 hours**

**Exit criteria:**
- [ ] pg_trickle works correctly under PgBouncer transaction-mode pooling
- [ ] Extension upgrade path tested (`0.7.0 → 0.8.0`)

---

## v0.9.0 — Observability, Anomaly Detection & pg_dump Support

**Goal:** Prometheus/Grafana observability, protect against anomalous change
spikes with a configurable fuse, and complete the pg_dump round-trip story.
After this milestone the product is externally visible and monitored.

### Observability

> **In plain terms:** Adds ready-made dashboards and metrics exports so you
> can see pg_trickle's health in existing monitoring systems without
> building them yourself. The Grafana dashboard gives you a live graph of
> refresh latency, how "stale" each stream table is, and how far behind CDC
> is — the same kind of operational visibility you'd expect from any
> production component.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| M1 | Prometheus exporter configuration guide | 4–6h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §1 |
| M2 | Grafana dashboard (refresh latency, staleness, CDC lag) | 4–6h | [PLAN_ECO_SYSTEM.md §1](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

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
| NAT-DUMP | `generate_dump()` + `restore_stream_tables()` companion functions; event trigger on extension load for orphaned catalog entries | 3–4d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §pg_dump |
| NAT-TEST | E2E tests: pg_dump round-trip, restore from backup, orphaned-entry recovery | 2–3d | [PLAN_NATIVE_SYNTAX.md](plans/sql/PLAN_NATIVE_SYNTAX.md) §pg_dump |

> **pg_dump support subtotal: ~5–7 days**

> **v0.9.0 total: ~58–82 hours**

**Exit criteria:**
- [ ] Grafana dashboard published
- [ ] Fuse triggers on configurable change-count threshold; `reset_fuse()` recovers
- [ ] pg_dump round-trip produces valid, restorable SQL for stream tables
- [ ] `ALTER EXTENSION pg_trickle UPDATE` tested (`0.8.0 → 0.9.0`)
- [ ] All public documentation current and reviewed

---

## v0.10.0 — Incremental Aggregate Maintenance

**Goal:** Implement algebraic incremental maintenance for decomposable aggregates
(COUNT, SUM, AVG, MIN, MAX, STDDEV), reducing per-group refresh from O(group_size)
to O(1) for the common case. This is the highest-potential-payoff item in the
performance plan — benchmarks show aggregate scenarios going from 2.5 ms to sub-1 ms
per group.

### Algebraic Aggregate Shortcuts (B-1)

> **In plain terms:** When only one row changes in a group of 100,000, today
> pg_trickle re-scans all 100,000 rows to recompute the aggregate. Algebraic
> maintenance keeps running totals: `new_sum = old_sum + Δsum`, `new_count =
> old_count + Δcount`. Only MIN/MAX needs a rescan — and only when the deleted
> value *was* the current minimum or maximum.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| B1-1 | Algebraic rules: COUNT, SUM, AVG, STDDEV (Welford online algorithm), MIN/MAX with rescan guard when deleted value equals current extremum | 3–4 wk | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-2 | Auxiliary column management (`__pgt_aux_count`, `__pgt_aux_sum`, etc.); view wrapper to hide from user queries | 1–2 wk | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-3 | Migration story for existing aggregate stream tables; periodic full-group recomputation to reset floating-point drift | 1 wk | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-4 | Fallback to full-group recomputation for non-decomposable aggregates (`mode`, percentile, `string_agg` with ordering) | 1 wk | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |
| B1-5 | Property-based tests: MIN/MAX boundary case (deleting the exact current min or max value must trigger rescan) | 1 wk | [PLAN_NEW_STUFF.md §B-1](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ Critical: the MIN/MAX maintenance rule is directionally tricky. The correct
> condition for triggering a rescan is: deleted value **equals** the current min/max
> (not when it differs). Getting this backwards silently produces stale aggregates
> on the most common OLTP delete pattern. See the corrected table and risk analysis
> in PLAN_NEW_STUFF.md §B-1.

> **Retraction consideration (B-1):** Keep in v0.10.0, but item B1-5 (property-based
> tests covering the MIN/MAX boundary case) is a **hard prerequisite** for B1-1, not
> optional follow-on work. The MIN/MAX rule was stated backwards in the original spec;
> the corrected rule is now in PLAN_NEW_STUFF.md. Do not merge any MIN/MAX algebraic
> path until property-based tests confirm: (a) deleting the exact current min triggers
> a rescan and (b) deleting a non-min value does not. Floating-point drift reset
> (B1-3) is also required before enabling persistent auxiliary columns.

> **Algebraic aggregates subtotal: ~7–9 weeks**

> **v0.10.0 total: ~7–9 weeks**

**Exit criteria:**
- [ ] COUNT/SUM/AVG/STDDEV algebraic paths implemented and benchmarked vs. full-group recompute
- [ ] MIN/MAX boundary case (delete-the-extremum) covered by property-based tests
- [ ] Auxiliary columns hidden from user queries via view wrapper
- [ ] Migration path for existing aggregate stream tables tested
- [ ] Floating-point drift reset mechanism in place (periodic recompute)
- [ ] Extension upgrade path tested (`0.9.0 → 0.10.0`)

---

## v0.11.0 — Partitioned Stream Tables & Operational Scale

**Goal:** Enable stream table storage to be declaratively partitioned (scope MERGE
to affected partitions for 100× I/O reduction on large tables), make the DAG
rebuild incremental for large multi-ST deployments, and add per-database worker
quotas for multi-tenant environments.

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

### Incremental DAG Rebuild (C-2)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C2-1 | Replace single `pgt_id` scalar in shared memory with a bounded ring buffer of affected IDs; full-rebuild fallback on overflow | 1 wk | [PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |
| C2-2 | Incremental topo-sort on affected subgraph; cache sorted schedule in shared memory | 1–2 wk | [PLAN_NEW_STUFF.md §C-2](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ A single `pgt_id` scalar in shared memory is vulnerable to overwrite when two DDL
> changes arrive between scheduler ticks — use a ring buffer or fall back to full rebuild.
> See PLAN_NEW_STUFF.md §C-2 risk analysis.

> **Incremental DAG rebuild subtotal: ~2–3 weeks**

### Multi-Database Scheduler Isolation (C-3)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| C3-1 | Per-database worker quotas (`pg_trickle.per_database_worker_quota`); priority ordering (IMMEDIATE > Hot > Warm > Cold); burst capacity up to 150% when other DBs are under budget | 2–3 wk | [PLAN_NEW_STUFF.md §C-3](plans/performance/PLAN_NEW_STUFF.md) |

> **Multi-DB isolation subtotal: ~2–3 weeks**

> **v0.11.0 total: ~9–13 weeks**

**Exit criteria:**
- [ ] Declaratively partitioned stream tables accepted; partition key tracked in catalog
- [ ] Partition-scoped MERGE benchmark: 10M-row ST, 0.1% change rate (expect ~100× I/O reduction)
- [ ] Ring-buffer DAG invalidation safe under rapid consecutive DDL changes (property-based test)
- [ ] Per-database worker quotas enforced; burst reclaimed within 1 scheduler cycle
- [ ] Extension upgrade path tested (`0.10.0 → 0.11.0`)

---

## v0.12.0 — Multi-Source Delta Batching, CDC Research & PG Backward Compatibility

**Goal:** Implement multi-source delta merging for join queries where multiple
source tables change simultaneously, conduct a formal research spike for
the custom logical decoding output plugin (D-2) before committing to a full
implementation, and widen the deployment target to PG 16–18.

### Multi-Table Delta Batching (B-3)

> **In plain terms:** When a join query has three source tables and all three
> change in the same cycle, today pg_trickle makes three separate passes through
> the source tables. B-3 merges those passes into one and prunes UNION ALL
> branches for sources with no changes.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| B3-1 | Intra-query delta-branch pruning: skip UNION ALL branch entirely when a source has zero changes in this cycle | 1–2 wk | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |
| B3-2 | Merged-delta generation: weight aggregation (`GROUP BY __pgt_row_id, SUM(weight)`) for cross-source deduplication; remove zero-weight rows | 3–4 wk | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |
| B3-3 | Property-based correctness tests for simultaneous multi-source changes; diamond-flow scenarios | 1–2 wk | [PLAN_NEW_STUFF.md §B-3](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ Cross-delta deduplication **must use weight aggregation (`SUM(weight)` grouped by
> `__pgt_row_id`), not `DISTINCT ON`**. `DISTINCT ON` silently discards corrections
> that should be summed and will produce wrong data for diamond-flow queries — the
> exact scenario this feature targets. Do not merge B3-2 without passing property-based
> correctness proofs. See PLAN_NEW_STUFF.md §B-3 risk analysis.

> **Retraction candidate (B-3):** The spec contains a correctness bug at the design level
> — the originally proposed `DISTINCT ON` deduplication produces silently wrong results
> for diamond-flow delta paths. The corrected approach (Z-set weight aggregation) is
> conceptually sound but requires formal correctness proofs or exhaustive property-based
> tests before any code is written. The risk rating is **Very High**. Recommend moving
> B-3 out of the numbered roadmap into a **post-1.0 research backlog** unless a formal
> correctness proof for the weight-aggregation path exists prior to v0.12.0 scoping.

> **Multi-source delta batching subtotal: ~5–8 weeks**

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

> **v0.12.0 total: ~13–19 weeks**

**Exit criteria:**
- [ ] Intra-query delta-branch pruning: zero-change sources produce no UNION ALL branch
- [ ] Merged-delta path passes property-based correctness tests for simultaneous multi-source changes
- [ ] D-2 spike: prototype exists; SPI-in-commit-callback constraint validated; RFC written
- [ ] PG 16 and PG 17 pass full E2E suite (trigger CDC mode)
- [ ] WAL decoder validated against PG 16–17 `pgoutput` format
- [ ] CI matrix covers PG 16, 17, 18
- [ ] Extension upgrade path tested (`0.11.0 → 0.12.0`)

---

## v0.13.0 — Core Refresh Optimizations, Scalability Foundations & UNLOGGED Buffers

**Goal:** Deliver the second and third waves of performance optimizations —
index-aware MERGE, predicate pushdown, change buffer compaction, cost-based
refresh strategy, columnar change tracking, tiered scheduling, and shared
change buffers — alongside opt-in UNLOGGED change buffers for reduced WAL
amplification.

### Core Refresh Optimizations (Wave 2)

> Read the risk analyses in
> [PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) before implementing.
> Implement in this order: A-4 (no schema change), B-2, C-4, then B-4.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-4 | Index-Aware MERGE Planning — planner hint injection (`enable_seqscan = off` for small-delta / large-target); covering index auto-creation on `__pgt_row_id` | 1–2 wk | [PLAN_NEW_STUFF.md §A-4](plans/performance/PLAN_NEW_STUFF.md) |
| B-2 | Delta Predicate Pushdown — push WHERE predicates from defining query into change-buffer `delta_scan` CTE; `OR old_col` handling for deletions; 5–10× delta-row-volume reduction for selective queries | 2–3 wk | [PLAN_NEW_STUFF.md §B-2](plans/performance/PLAN_NEW_STUFF.md) |
| C-4 | Change Buffer Compaction — net-change compaction (INSERT+DELETE=no-op; UPDATE+UPDATE=single row); run when buffer exceeds `pg_trickle.compact_threshold`; use advisory lock to serialise with refresh | 2–3 wk | [PLAN_NEW_STUFF.md §C-4](plans/performance/PLAN_NEW_STUFF.md) |
| B-4 | Cost-Based Refresh Strategy — replace fixed `differential_max_change_ratio` with a history-driven cost model fitted on `pgt_refresh_history`; cold-start fallback to fixed threshold | 2–3 wk | [PLAN_NEW_STUFF.md §B-4](plans/performance/PLAN_NEW_STUFF.md) |

> ⚠️ C-4: The compaction DELETE **must use `seq` (the sequence primary key) not `ctid`** as
> the stable row identifier. `ctid` changes under VACUUM and will silently delete the wrong
> rows. See the corrected SQL and risk analysis in PLAN_NEW_STUFF.md §C-4.

> **Core refresh optimizations subtotal: ~60–130h (A-4, B-2, C-4, B-4)**

### Scalability Foundations (Wave 3)

> Items from [PLAN_NEW_STUFF.md](plans/performance/PLAN_NEW_STUFF.md) Wave 3. Read risk
> analyses before implementing — particularly C-1's read-tracking pitfall.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| A-2 | Columnar Change Tracking — per-column bitmask in CDC triggers; skip rows where no referenced column changed; lightweight UPDATE-only path when only projected columns changed; 50–90% delta-volume reduction for wide-table UPDATE workloads | 3–4 wk | [PLAN_NEW_STUFF.md §A-2](plans/performance/PLAN_NEW_STUFF.md) |
| C-1 | Tiered Refresh Scheduling — Hot/Warm/Cold/Frozen tier classification; lazy refresh for Cold/Frozen STs; configurable per-ST tier override; 80% scheduler-CPU reduction in large deployments | 3–4 wk | [PLAN_NEW_STUFF.md §C-1](plans/performance/PLAN_NEW_STUFF.md) |
| D-4 | Shared Change Buffers — single buffer per source shared across all dependent STs; multi-frontier cleanup coordination; static-superset column mode for initial implementation | 3–4 wk | [PLAN_NEW_STUFF.md §D-4](plans/performance/PLAN_NEW_STUFF.md) |

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

> **Scalability foundations subtotal: ~60–120h**

### UNLOGGED Change Buffers — Opt-In (D-1)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| D-1 | UNLOGGED Change Buffers — create change buffers as `UNLOGGED` to reduce CDC WAL amplification; `pg_trickle.unlogged_buffers` GUC (default `false`, opt-in); crash recovery and standby promotion trigger FULL refresh | 1–2 wk | [PLAN_NEW_STUFF.md §D-1](plans/performance/PLAN_NEW_STUFF.md) |

> Default flipped to `false` (opt-in only) to avoid forced FULL
> refreshes on all stream tables for users who have not explicitly accepted the
> crash/standby tradeoff.

> **D-1 subtotal: ~1–2 weeks**

> **v0.13.0 total: ~16–31 weeks**

**Exit criteria:**
- [ ] A-4: Covering index auto-created on `__pgt_row_id`; planner hint prevents seq-scan on small delta
- [ ] B-2: Predicate pushdown reduces delta volume for selective queries (E2E benchmark)
- [ ] C-4: Compaction uses `seq` PK; correct under concurrent VACUUM; serialised with advisory lock
- [ ] B-4: Cost model self-calibrates from refresh history; correctly selects FULL for join_agg at 10% change rate
- [ ] A-2: Bitmask skips irrelevant rows; UPDATE-only path reduces delta volume (benchmarked)
- [ ] C-1: Tier classification uses delta-based read tracking; Cold STs skip refresh correctly
- [ ] D-4: Shared buffer serves multiple STs; multi-frontier cleanup prevents premature deletion
- [ ] D-1: UNLOGGED change buffers opt-in (`unlogged_buffers = false` by default); crash-recovery FULL-refresh path tested
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

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TS1 | sqllogictest: run PostgreSQL sqllogic suite through pg_trickle DIFFERENTIAL mode | 2–3d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS2 | JOB (Join Order Benchmark): correctness baseline and refresh latency profiling | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS3 | Nexmark streaming benchmark: sustained high-frequency DML correctness | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |

> **External test suites subtotal: ~4–7 days**

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
| R2 | PGXN / apt / rpm packaging | 8–12h | [PLAN_PACKAGING.md](plans/infra/PLAN_PACKAGING.md) |
| R3 | ~~Docker Hub official image~~ → CNPG extension image | ✅ Done | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |
| R4 | CNPG operator hardening (K8s 1.33+ native ImageVolume) | 4–6h | [PLAN_CLOUDNATIVEPG.md](plans/ecosystem/PLAN_CLOUDNATIVEPG.md) |

> **v1.0.0 total: ~18–27 hours**

**Exit criteria:**
- [ ] Published on PGXN and Docker Hub
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
| A1 | Circular dependency support (SCC fixpoint iteration) | ~40h | [CIRCULAR_REFERENCES.md](plans/sql/CIRCULAR_REFERENCES.md) |
| A2 | Transactional IVM Phase 4 remaining (ENR-based transition tables, aggregate fast-path, C-level triggers, prepared stmt reuse) | ~36–54h | [PLAN_TRANSACTIONAL_IVM.md](plans/sql/PLAN_TRANSACTIONAL_IVM.md) |
| A3 | PostgreSQL 19 forward-compatibility | TBD | [PLAN_PG19_COMPAT.md](plans/infra/PLAN_PG19_COMPAT.md) |
| A4 | PostgreSQL 14–15 backward compatibility | ~40h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) |
| A5 | Partitioned stream table storage (opt-in) | ~60–80h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §4 |
| A6 | Buffer table partitioning by LSN range (`pg_trickle.buffer_partitioning` GUC) | ~3–4d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 4 §3.3 |
| A7 | Skip-unchanged-column scanning in delta SQL (requires column-usage demand-propagation pass in DVM parser) | ~1–2d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 4 §3.4 |
| A8 | `ROWS FROM()` with multiple SRF functions — very low demand, deferred | ~1–2d | [PLAN_TRANSACTIONAL_IVM_PART_2.md](plans/sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) Task 2.3 |

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
| v0.6.0 — Partitioning, Idempotent DDL & Circular Dependency Foundation | ~35–50h | 331–493h | |
| v0.7.0 — Watermarks & Circular DAG Execution | ~36–39h | 367–532h | |
| v0.8.0 — Connection Pooler Compatibility | ~56–80h | 423–612h | |
| v0.9.0 — Observability, Anomaly Detection & pg_dump Support | ~58–82h | 481–694h | |
| v0.10.0 — Incremental Aggregate Maintenance (B-1) | ~7–9 wk | — | |
| v0.11.0 — Partitioned Stream Tables & Operational Scale (A-1, C-2, C-3) | ~9–13 wk | — | |
| v0.12.0 — Multi-Source Delta Batching, CDC Research & PG Backward Compat | ~13–19 wk | — | |
| v0.13.0 — Core Refresh Opt., Scalability Foundations & UNLOGGED Buffers | ~16–31 wk | — | |
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
