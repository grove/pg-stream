# PLAN_EC_TI — Combined Implementation Order

**Source plans:**
- [PLAN_EDGE_CASES.md](PLAN_EDGE_CASES.md) (EC-xx)
- [sql/PLAN_TRANSACTIONAL_IVM_PART_2.md](sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) (Part 2)

**Date:** 2026-03-04  
**Last updated:** 2026-03-04  
**Status:** Stages 1–2 COMPLETE (incl. bug fixes + test coverage).
Next up: Stage 3 (SQL Coverage Expansion).  
**Principle:** No SQL-surface expansion while P0 correctness bugs are open.

---

## Stage 1 — P0 Correctness (PLAN_EDGE_CASES Sprint 1)

Users can silently get wrong results from these bugs today. Nothing else
ships until all three are resolved and TPC-H regression passes.

| # | Item | Effort | Gate | Status |
|---|------|--------|------|--------|
| 1 | **EC-19** — Reject WAL + keyless without REPLICA IDENTITY FULL at creation time | 0.5 day | — | ✅ Done |
| 2 | **EC-06** — Keyless tables: net-counting delta + counted DELETE + non-unique index | 2–3 days | EC-19 done | ✅ Done + 2 bug fixes |
| 3 | **EC-01** — R₀ via EXCEPT ALL: split Part 1 of `diff_inner_join`, `diff_left_join`, `diff_full_join` to use pre-change right state | 4–6 days | EC-06 done | ✅ Done — Part 1 split into 1a (inserts ⋈ R₁) + 1b (deletes ⋈ R₀); Part 3 split for outer/full joins; 8 unit tests |

**Completion gate:** `just test-all` green + TPC-H Q07 passes with `TPCH_CYCLES=5`.

**Stage 1 is COMPLETE.** All three P0 correctness bugs are resolved:
- EC-19: WAL+keyless rejection at creation time.
- EC-06: Full keyless support via `has_keyless_source` catalog flag, non-unique
  index, net-counting delta SQL (decompose → SUM → generate_series), counted
  DELETE (ROW_NUMBER matching), plain INSERT, forced explicit DML path.
  **Post-impl bug fixes:** (a) Guard trigger `RETURN NEW` → `RETURN OLD` for
  DELETE ops (NULL return silently cancelled managed-refresh DELETEs);
  (b) Keyless UPDATE template changed from no-op to real UPDATE (no-op broke
  aggregate queries on keyless sources where 'I'-action rows update groups).
  **Test coverage:** 7 E2E tests in `e2e_keyless_duplicate_tests.rs` (all passing).
- EC-01: R₀ via EXCEPT ALL for inner/left/full joins.

---

## Stage 2 — P1 Operational Safety (PLAN_EDGE_CASES Sprint 2)

Low-effort guard-rail fixes that prevent users from inadvertently
corrupting state. Ship as a batch before expanding SQL coverage.

| # | Item | Effort | Status |
|---|------|--------|--------|
| 4 | **EC-25** — BEFORE TRUNCATE guard trigger on stream tables | 0.5 day | ✅ Done (1 E2E test) |
| 5 | **EC-26** — BEFORE I/U/D guard trigger on stream tables | 1 day | ✅ Done (4 E2E tests) |
| 6 | **EC-15** — WARNING at creation time for `SELECT *` | 0.5 day | ✅ Done (10 unit tests) |
| 7 | **EC-11** — `scheduler_falling_behind` NOTIFY alert (80%) | 1 day | ✅ Done (monitor unit tests) |
| 8 | **EC-13** — Default `diamond_consistency` to `'atomic'` | 0.5 day | ✅ Done (9+ E2E tests) |
| 9 | **EC-18** — Rate-limited LOG for `auto` CDC stuck in TRIGGER | 1 day | ✅ Done |
| 10 | **EC-34** — Auto-detect missing WAL slot; fallback to TRIGGER | 1 day | ✅ Done |

**Completion gate:** `just test-all` green.

**Stage 2 hardening (post-impl):**
- `internal_refresh` GUC flag now set in ALL refresh code paths:
  `execute_manual_refresh`, `execute_manual_full_refresh`,
  `execute_full_refresh`, `execute_topk_refresh`, `pgt_ivm_handle_truncate`.
  Prevents guard triggers from blocking any managed refresh operation.
- Guard trigger returns `OLD` for DELETE (not `NEW` which is NULL).
- EC-15 detection logic extracted to pure `detect_select_star()` function
  for unit testability (10 tests covering all patterns).

---

## Stage 3 — SQL Coverage Expansion (Part 2 Phases 1–2)

Expand the SQL surface area. All work is in the parser/rewrite layer and
the DVM operator set — no CDC or scheduler changes.

### Phase 1 — Auto-Rewrite Completions (Part 2)

| # | Item | Effort |
|---|------|--------|
| 11 | **Part 2 Task 1.1** — Mixed UNION / UNION ALL: `rewrite_mixed_union()` pass | 2–3 days |
| 12 | **Part 2 Task 1.2** — Multiple PARTITION BY: `rewrite_multi_partition()` pass | 2–3 days |
| 13 | **Part 2 Task 1.3 / EC-03** — Nested window expressions: `rewrite_nested_window_exprs()` subquery lift | 3–5 days |

### Phase 2 — New DVM Operators (Part 2)

| # | Item | Effort |
|---|------|--------|
| 14 | **Part 2 Task 2.1 / EC-32** — `ALL (subquery)` → `NOT EXISTS (… EXCEPT …)` rewrite | 2–3 days |
| 15 | **Part 2 Task 2.2** — Deeply nested SubLinks in OR: extend `rewrite_sublinks_in_bool_tree()` | 2–3 days |
| 16 | **Part 2 Task 2.3** — `ROWS FROM()` with multiple functions: rewrite to `unnest` / LATERAL zip | 1–2 days |
| 17 | **Part 2 Task 2.4** — LATERAL with RIGHT/FULL JOIN: update error message only (PostgreSQL-level constraint) | 0.5 day |

**Completion gate:** `just test-all` green + new E2E tests from each task passing.

---

## Stage 4 — P1 Remainder + Trigger-Level Optimisations

These can proceed in parallel if bandwidth allows (different files: EC-16
touches `src/refresh.rs`; Part 2 Phase 3 touches `src/cdc.rs`).

### EC-16 (PLAN_EDGE_CASES P1 remainder)

| # | Item | Effort |
|---|------|--------|
| 18 | **EC-16** — `pg_proc` hash polling for undetected ALTER FUNCTION changes | 2 days |

### Part 2 Phase 3 — Trigger-Level Optimisations

| # | Item | Effort |
|---|------|--------|
| 19 | **Part 2 Task 3.1** — Column-level change detection for UPDATE: `changed_cols` bitmask in buffer | 3–4 days |
| 20 | **Part 2 Task 3.2** — Incremental TRUNCATE: negation delta for simple single-source stream tables | 2–3 days |
| 21 | **Part 2 Task 3.3** — Buffer table partitioning by LSN range: `pg_trickle.buffer_partitioning` GUC | 3–4 days |
| 22 | **Part 2 Task 3.4** — Skip-unchanged-column scanning in delta SQL | 1–2 days |
| 23 | **Part 2 Task 3.5** — Online ADD COLUMN without full reinit | 2–3 days |

**Note on ordering within Phase 3:** Task 3.1 must land before 3.4 (3.4
depends on the `changed_cols` bitmask). Tasks 3.2, 3.3, and 3.5 are
independent of each other.

**Completion gate:** `just test-all` green + buffer-size benchmarks showing
≥ 30% UPDATE buffer reduction for a 10-column table.

---

## Stage 5 — Remaining Aggregate Coverage (Part 2 Phase 4)

Pure additions. Independent of everything above.

| # | Item | Effort |
|---|------|--------|
| 24 | **Part 2 Task 4.1** — Regression aggregates: `CORR`, `COVAR_*`, `REGR_*` via group-rescan | 2–3 days |
| 25 | **Part 2 Task 4.2** — Hypothetical-set aggregates: `RANK()`, `DENSE_RANK()`, `PERCENT_RANK()`, `CUME_DIST()` WITHIN GROUP | 1–2 days |
| 26 | **Part 2 Task 4.3** — `XMLAGG` via group-rescan | 0.5 day |

**Completion gate:** `just test-all` green + 36+ aggregate functions confirmed in unit tests.

---

## Stage 6 — IMMEDIATE Mode Parity (Part 2 Phase 5)

Highest-risk work due to transaction and stack-depth interactions. Leave
until the engine is stable post-Stage 5.

| # | Item | Effort |
|---|------|--------|
| 27 | **Part 2 Task 5.1 / EC-09** — Recursive CTEs in IMMEDIATE mode: validate semi-naive with `DeltaSource::TransitionTable`; add stack-depth guard | 2–3 days |
| 28 | **Part 2 Task 5.2 / EC-09** — TopK in IMMEDIATE mode: statement-level micro-refresh; `ivm_topk_max_limit` GUC | 2–3 days |

**Completion gate:** `just test-all` green + dedicated IMMEDIATE + recursive CTE E2E test passing.

---

## Stage 7 — P2 Usability Gaps + P3 Documentation Sweep

| # | Item | Effort |
|---|------|--------|
| 29 | **EC-05** — Foreign table polling-based change detection | 2–3 days |
| 30 | **EC-02** — `pg_trickle.max_grouping_set_branches` GUC | 0.5 day |
| 31 | **EC-20** — Post-restart CDC TRANSITIONING health check | 1 day |
| 32 | **EC-28** — PgBouncer configuration documentation | 0.5 day |
| 33 | **EC-17** — DDL-during-refresh documentation clarification | 0.5 day |
| 34 | **EC-21/22/23** — Replication / standby limitations documentation sweep | 0.5 day |
| 35 | All remaining P3 items | — |

---

## Summary Table

| Stage | Source | Items | Estimated total effort |
|-------|--------|-------|------------------------|
| 1 — P0 Correctness | PLAN_EDGE_CASES | 3 | 7–10 days |
| 2 — P1 Safety | PLAN_EDGE_CASES | 7 | 5–6 days |
| 3 — SQL Coverage | Part 2 Ph 1–2 | 7 | 13–19 days |
| 4 — P1 Remainder + Triggers | EC-16 + Part 2 Ph 3 | 6 | 13–18 days |
| 5 — Aggregates | Part 2 Ph 4 | 3 | 4–6 days |
| 6 — IMMEDIATE Parity | Part 2 Ph 5 | 2 | 4–6 days |
| 7 — Usability + Docs | PLAN_EDGE_CASES P2/P3 | 7+ | 5–7 days |
| **Total** | | **35+** | **~51–72 days** |
