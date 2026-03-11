# pg_trickle — Project Roadmap

> **Last updated:** 2026-03-11
> **Latest release:** 0.2.3 (scheduled)
> **Current milestone:** v0.3.0 — DVM Correctness, SAST & Test Coverage

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
 │Released│─│Released│─│Released│─│Released│─│Mode & │─│DVM Fix │─│Parallel│─│  RLS  │─│Partition│
 │ ✅      │ │ ✅      │ │ ✅      │ │ ✅      │ │Ops Gap│ │SAST&TPC│ │Refresh │ │       │ │Support  │
 └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘ └────────┘
      │
      └─ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
         │ 0.7.0  │ │ 0.8.0  │ │ 0.9.0  │ │ 1.0.0  │ │ 1.x+   │
         │PG 16-18│─│Pooler &│─│Observ. │─│Stable  │─│Scale & │
         │Compat  │ │ExtTests│ │& Integ.│ │Release │ │Ecosys. │
         └────────┘ └────────┘ └────────┘ └────────┘ └────────┘
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

Core implementation is complete (parser, catalog, refresh path, docs, 9 E2E
tests). The `topk_offset` catalog column shipped in v0.2.1 and is exercised
by the paged TopK feature here.

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| OS1 | 9 OFFSET E2E tests in `e2e_topk_tests.rs` | ✅ Done | [PLAN_OFFSET_SUPPORT.md](plans/sql/PLAN_OFFSET_SUPPORT.md) §Step 6 |
| OS2 | `sql/pg_trickle--0.2.1--0.2.2.sql` — function signature updates (no schema DDL needed) | ✅ Done | [PLAN_OFFSET_SUPPORT.md](plans/sql/PLAN_OFFSET_SUPPORT.md) §Step 2 |

### AUTO Refresh Mode ✅

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| AM1 | `RefreshMode::Auto` — uses DIFFERENTIAL when supported, falls back to FULL | ✅ Done | [PLAN_REFRESH_MODE_DEFAULT.md](plans/sql/PLAN_REFRESH_MODE_DEFAULT.md) |
| AM2 | `create_stream_table` default changed from `'DIFFERENTIAL'` to `'AUTO'` | ✅ Done | — |
| AM3 | `create_stream_table` schedule default changed from `'1m'` to `'calculated'` | ✅ Done | — |

### ALTER QUERY ✅

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| AQ1 | `alter_stream_table(query => ...)` — validate, classify schema change, migrate storage | ✅ Done | [PLAN_ALTER_QUERY.md](plans/PLAN_ALTER_QUERY.md) |
| AQ2 | Schema classification: same, compatible (ADD/DROP COLUMN), incompatible (full rebuild) | ✅ Done | — |
| AQ3 | ALTER-aware cycle detection (`check_for_cycles_alter`) | ✅ Done | — |
| AQ4 | CDC dependency migration (add/remove triggers for changed sources) | ✅ Done | — |
| AQ5 | SQL Reference & CHANGELOG documentation | ✅ Done | — |

### Upgrade Tooling ✅

| Item | Description | Status | Ref |
|------|-------------|--------|-----|
| UG1 | Version mismatch check — scheduler warns if `.so` version ≠ SQL version | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) §5.2 |
| UG2 | FAQ upgrade section — 3 new entries with UPGRADING.md cross-links | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) §5.4 |
| UG3 | CI and local upgrade automation now target 0.2.2 (`upgrade-check`, upgrade-image defaults, upgrade E2E env) | ✅ Done | [PLAN_UPGRADE_MIGRATIONS.md](plans/sql/PLAN_UPGRADE_MIGRATIONS.md) |

### IMMEDIATE Mode Parity ✅

Close the gap between DIFFERENTIAL and IMMEDIATE mode SQL coverage for the
two remaining high-risk patterns — recursive CTEs and TopK queries.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| IM1 | Validate recursive CTE semi-naive in IMMEDIATE mode; add stack-depth guard for deeply recursive defining queries | 2–3d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 6 §5.1 | ✅ Done — `check_for_delete_changes` handles `TransitionTable`; `generate_change_buffer_from` uses NEW transition table in IMMEDIATE mode; `ivm_recursive_max_depth` GUC (default 100) injects `__pgt_depth` counter into semi-naive SQL |
| IM2 | TopK in IMMEDIATE mode: statement-level micro-refresh + `ivm_topk_max_limit` GUC | 2–3d | [PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md](plans/PLAN_EDGE_CASES_TIVM_IMPL_ORDER.md) Stage 6 §5.2 | ✅ Done — `apply_topk_micro_refresh()` in ivm.rs; GUC threshold check in api.rs; 10 E2E tests (basic, insert, delete, update, aggregate, offset, multi-DML, threshold rejection, mode switch) |

> **IMMEDIATE parity subtotal: ✅ Complete (IM1 + IM2)**

### Edge Case Hardening ✅

Self-contained items from Stage 7 of the edge-cases/TIVM implementation plan.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| EC1 | `pg_trickle.max_grouping_set_branches` GUC — cap CUBE/ROLLUP branch-count explosion | 4h | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-02 | ✅ Done — GUC in config.rs (default 64, range 1–65536); parser.rs rejects when branch count exceeds limit; 3 E2E tests (rejection, within-limit, raised limit) |
| EC2 | Post-restart CDC `TRANSITIONING` health check — detect stuck CDC transitions after crash or restart | 1d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-20 | ✅ Done — `check_cdc_transition_health()` in scheduler.rs; detects missing replication slots; rolls back to TRIGGER mode |
| EC3 | Foreign table support: polling-based change detection via periodic re-execution | 2–3d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-05 | ✅ Done — `pg_trickle.foreign_table_polling` GUC; `setup_foreign_table_polling()` creates snapshot table; `poll_foreign_table_changes()` uses EXCEPT ALL deltas; 3 E2E tests (rejection, FULL mode, polling correctness) |

> **Edge-case hardening subtotal: ✅ Complete (EC1 + EC2 + EC3)**

### Documentation Sweep

Remaining documentation gaps identified in Stage 7 of the gap analysis.

| Item | Description | Effort | Status | Ref |
|------|-------------|--------|--------|-----|
| DS1 | DDL-during-refresh behaviour: document safe patterns and races | 2h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-17 |
| DS2 | Replication/standby limitations: document in FAQ and Architecture | 3h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-21/22/23 |
| DS3 | PgBouncer configuration guide: session-mode requirements and known incompatibilities | 2h | ✅ Done | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-28 |

> **Documentation sweep subtotal: ✅ Complete**

### WAL CDC Hardening

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

**Status: Ready for release (tag pending).**

---

## v0.3.0 — DVM Correctness, SAST & Test Coverage

**Goal:** Re-enable all 18 previously-ignored DVM correctness E2E tests by
fixing HAVING, FULL OUTER JOIN, correlated EXISTS+HAVING, and correlated scalar
subquery differential computation bugs. Harden the SAST toolchain with
privilege-context rules and an unsafe-block baseline. Expand TPC-H coverage
with rollback, mode-comparison, single-row, and DAG tests.

### DVM Correctness Fixes

| Item | Description | Status |
|------|-------------|--------|
| DC1 | HAVING clause differential correctness — fix `COUNT(*)` rewrite and threshold-crossing upward rescan (5 tests un-ignored) | ✅ Done |
| DC2 | FULL OUTER JOIN differential correctness — fix row-id mismatch, compound GROUP BY expressions, SUM NULL semantics, and rescan CTE SELECT list (5 tests un-ignored) | ✅ Done |
| DC3 | Correlated EXISTS with HAVING differential correctness — fix EXISTS sublink parser discarding GROUP BY/HAVING, row-id mismatch for `Project(SemiJoin)`, and `diff_project` row-id recomputation (1 test un-ignored) | ✅ Done |
| DC4 | Correlated scalar subquery differential correctness — `rewrite_correlated_scalar_in_select` rewrites correlated scalar subqueries to LEFT JOINs before DVM parsing (2 tests un-ignored) | ✅ Done |

> **DVM correctness subtotal: 18 previously-ignored E2E tests re-enabled (0 remaining)**

### SAST Program (Phases 1–3)

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
- [ ] Extension upgrade path tested (`0.2.3 → 0.3.0`)

---

## v0.4.0 — Parallel Refresh

**Goal:** Deliver true parallel refresh via a coordinator/worker model,
improving refresh throughput and freshness latency for independent stream
tables. This milestone is expected to materially reduce end-to-end staleness
for large DAGs but does not raise source-table write throughput on the current
trigger-based CDC path.

### Parallel Refresh

Detailed implementation is tracked in
[PLAN_PARALLELISM.md](plans/sql/PLAN_PARALLELISM.md). The older
[REPORT_PARALLELIZATION.md](plans/performance/REPORT_PARALLELIZATION.md)
remains the options-analysis precursor.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| P1 | Phase 0–1: instrumentation, `dry_run`, and execution-unit DAG (atomic groups + IMMEDIATE closures) | 12–20h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |
| P2 | Phase 2–4: job table, worker budget, dynamic refresh workers, and ready-queue dispatch | 16–28h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |
| P3 | Phase 5–7: composite units, observability, rollout gating, and CI validation | 12–24h | [PLAN_PARALLELISM.md §10](plans/sql/PLAN_PARALLELISM.md) |

> **Parallel refresh subtotal: ~40–72 hours**

**Exit criteria:**
- [ ] `max_concurrent_refreshes` drives real parallel refresh via coordinator + dynamic refresh workers
- [ ] Extension upgrade path tested (`0.3.0 → 0.4.0`)

---

## v0.5.0 — Row-Level Security (RLS)

**Goal:** Harden the security context for stream tables and IVM triggers so the
extension is safe to deploy in multi-tenant environments.

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

> **v0.5.0 total: ~8–12 hours**

**Exit criteria:**
- [ ] RLS semantics documented; change buffers RLS-hardened; IVM triggers SECURITY DEFINER
- [ ] RLS on stream table E2E-tested (DIFFERENTIAL + IMMEDIATE)
- [ ] Extension upgrade path tested (`0.4.0 → 0.5.0`)

---

## v0.6.0 — Partitioning Support (Source Tables)

**Goal:** Validate and harden partitioned source table support. Trigger-based
CDC already propagates to child partitions (PG 13+ trigger propagation), but
there are validation gaps, missing tests, and an ATTACH PARTITION detection
hole. Partitioned stream table *storage* is deferred to post-1.0.

### Partitioning Support (Source Tables)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PT1 | E2E tests for partitioned source tables (RANGE, basic CRUD, differential refresh) | 8–12h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §7 |
| PT2 | ATTACH PARTITION detection in DDL hook → force `needs_reinit` | 4–8h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.3 |
| PT3 | WAL publication: set `publish_via_partition_root = true` for partitioned sources | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §3.4 |
| PT4 | Foreign table source detection (`relkind = 'f'`) → restrict to FULL mode | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §6.3 |
| PT5 | Documentation: partitioned source table support & caveats | 2–4h | [PLAN_PARTITIONING_SHARDING.md](plans/infra/PLAN_PARTITIONING_SHARDING.md) §8 |

> **Partitioning subtotal: ~18–32 hours**

**Exit criteria:**
- [ ] Partitioned source tables E2E-tested; ATTACH PARTITION detected
- [ ] WAL mode works with `publish_via_partition_root = true`
- [ ] Extension upgrade path tested (`0.5.0 → 0.6.0`)

---

## v0.7.0 — PostgreSQL Backward Compatibility (PG 16–18)

**Goal:** Widen the deployment target from PG 18-only to PG 16–18. pgrx 0.17.0
supports PG 13–18 via feature flags. Starting with PG 16–18 minimizes scope
(only JSON_TABLE gating needed) while widening the deployment target. PG 14–15
support can follow in a later release.

### PostgreSQL Backward Compatibility (PG 16–18)

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| BC1 | Cargo.toml feature flags (`pg16`, `pg17`, `pg18`) + `cfg_aliases` | 4–8h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 1 |
| BC2 | `#[cfg]` gate JSON_TABLE nodes in `parser.rs` (~250 lines, PG 17+) | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 2 |
| BC3 | `pg_get_viewdef()` trailing-semicolon behavior verification | 2–4h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phase 3 |
| BC4 | CI matrix expansion (PG 16, 17, 18) + parameterized Dockerfiles | 12–16h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §5.2 Phases 4–5 |
| BC5 | WAL decoder validation against PG 16–17 `pgoutput` format | 8–12h | [PLAN_PG_BACKCOMPAT.md](plans/infra/PLAN_PG_BACKCOMPAT.md) §6A |

> **Backward compatibility subtotal: ~38–56 hours**

**Exit criteria:**
- [ ] PG 16 and PG 17 pass full E2E suite (trigger CDC mode)
- [ ] WAL decoder validated against PG 16–17 `pgoutput` format
- [ ] CI matrix covers PG 16, 17, 18
- [ ] Extension upgrade path tested (`0.6.0 → 0.7.0`)

---

## v0.8.0 — Connection Pooler Compatibility & External Test Suites

**Goal:** Enable cloud-native deployments by supporting PgBouncer
transaction-mode pooling (the default at RDS Proxy, Supabase, Neon). Validate
correctness against independent query corpora (sqllogictest, JOB, Nexmark)
beyond TPC-H. After this milestone the extension is suitable for production
use on mainstream cloud PostgreSQL deployments.

### Connection Pooler Compatibility

pg_trickle uses session-level advisory locks and `PREPARE` statements that are
incompatible with PgBouncer transaction-mode pooling. This section replaces all
session-scoped state with transaction-scoped equivalents.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| PB1 | Replace `pg_advisory_lock()` with `pg_advisory_xact_lock()` across refresh, CDC, and scheduler coordination | 3–4d | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) G8.4 |
| PB2 | Eliminate `PREPARE __pgt_merge_*` prepared statements (replace with inline or per-transaction SQL) | 3–4d | [GAP_SQL_PHASE_7.md](plans/sql/GAP_SQL_PHASE_7.md) G8.4 |
| PB3 | E2E validation against PgBouncer transaction-mode (Docker Compose with pooler sidecar) | 1–2d | [PLAN_EDGE_CASES.md](plans/PLAN_EDGE_CASES.md) EC-28 |

> **PgBouncer compatibility subtotal: ~7–10 days**

### External Test Suite Integration

Validate correctness against independent query corpora beyond TPC-H.

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| TS1 | sqllogictest: run PostgreSQL sqllogic suite through pg_trickle DIFFERENTIAL mode | 2–3d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS2 | JOB (Join Order Benchmark): correctness baseline and refresh latency profiling | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |
| TS3 | Nexmark streaming benchmark: sustained high-frequency DML correctness | 1–2d | [PLAN_TESTING_GAPS.md](plans/testing/PLAN_TESTING_GAPS.md) §J |

> **External test suites subtotal: ~4–7 days**

> **v0.8.0 total: ~160–208 hours**

**Exit criteria:**
- [ ] pg_trickle works correctly under PgBouncer transaction-mode pooling
- [ ] At least one external test corpus (sqllogictest, JOB, or Nexmark) passes
- [ ] Extension upgrade path tested (`0.7.0 → 0.8.0`)

---

## v0.9.0 — Observability & Integration

**Goal:** Prometheus/Grafana observability, dbt-pgtrickle formal release, and
complete documentation review. After this milestone the product is externally
visible and monitored.

### Observability

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| M1 | Prometheus exporter configuration guide | 4–6h | [PLAN_ECO_SYSTEM.md](plans/ecosystem/PLAN_ECO_SYSTEM.md) §1 |
| M2 | Grafana dashboard (refresh latency, staleness, CDC lag) | 4–6h | [PLAN_ECO_SYSTEM.md §1](plans/ecosystem/PLAN_ECO_SYSTEM.md) |

### Integration & Release prep

| Item | Description | Effort | Ref |
|------|-------------|--------|-----|
| I1 | dbt-pgtrickle 0.1.0 formal release (PyPI) | 2–3h | [dbt-pgtrickle/](dbt-pgtrickle/) · [PLAN_DBT_MACRO.md](plans/dbt/PLAN_DBT_MACRO.md) |
| I2 | Complete documentation review & polish | 4–6h | [docs/](docs/) |

> **v0.9.0 total: ~14–21 hours**

**Exit criteria:**
- [ ] Grafana dashboard published
- [ ] dbt-pgtrickle 0.1.0 on PyPI
- [ ] `ALTER EXTENSION pg_trickle UPDATE` tested (`0.8.0 → 0.9.0`)
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
- [ ] Upgrade path from v0.9.0 tested
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
| v0.3.0 — DVM Correctness, SAST & Test Coverage | ~20–30h | 185–252h | |
| v0.4.0 — Parallel Refresh | 40–72h | 225–324h | |
| v0.5.0 — Row-Level Security (RLS) | 8–12h | 233–336h | |
| v0.6.0 — Partitioning Support (Source Tables) | 18–32h | 251–368h | |
| v0.7.0 — PostgreSQL Backward Compatibility (PG 16–18) | 38–56h | 289–424h | |
| v0.8.0 — Connection Pooler Compatibility & External Test Suites | 160–208h | 449–632h | |
| v0.9.0 — Observability & Integration | 14–21h | 463–653h | |
| v1.0.0 — Stable release | 18–27h | 481–680h | |
| Post-1.0 (ecosystem) | 88–134h | 569–814h | |
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
