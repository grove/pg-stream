# PLAN_EC_TI — Combined Implementation Order

**Source plans:**
- [PLAN_EDGE_CASES.md](PLAN_EDGE_CASES.md) (EC-xx)
- [sql/PLAN_TRANSACTIONAL_IVM_PART_2.md](sql/PLAN_TRANSACTIONAL_IVM_PART_2.md) (Part 2)

**Date:** 2026-03-04  
**Last updated:** 2026-03-08  
**Status:** ALL STAGES COMPLETE. All deferred items resolved (Task 2.3 ROWS FROM implemented).
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

| # | Item | Effort | Status |
|---|------|--------|--------|
| 11 | **Part 2 Task 1.1** — Mixed UNION / UNION ALL | 2–3 days | ✅ **Already works natively** — `collect_union_children` + `Distinct`/`UnionAll` OpTree handle mixed arms; no SQL rewrite needed |
| 12 | **Part 2 Task 1.2** — Multiple PARTITION BY | 2–3 days | ✅ **Handled natively** — window group recomputation approach; `rewrite_multi_partition_windows()` exists but is unused; api.rs comment confirms no rewrite call needed |
| 13 | **Part 2 Task 1.3 / EC-03** — Nested window expressions | 3–5 days | ✅ **DONE** — `rewrite_nested_window_exprs()` subquery-lift in `src/dvm/parser.rs`; exported from `mod.rs`; wired in `api.rs` before DISTINCT ON rewrite |

### Phase 2 — New DVM Operators (Part 2)

| # | Item | Effort | Status |
|---|------|--------|--------|
| 14 | **Part 2 Task 2.1 / EC-32** — `ALL (subquery)` NULL-safe AntiJoin | 2–3 days | ✅ **DONE** — `parse_all_sublink()` in `src/dvm/parser.rs` updated to NULL-safe condition `(col IS NULL OR NOT (x op col))`; full AntiJoin pipeline was already wired |
| 15 | **Part 2 Task 2.2** — Deeply nested SubLinks in OR | 2–3 days | ✅ **DONE** — `flatten_and_conjuncts()` helper added; `and_contains_or_with_sublink()` now recurses through nested AND layers; `rewrite_and_with_or_sublinks()` uses flattened conjunct list — handles `AND(AND(OR(EXISTS(...))))` |
| 16 | **Part 2 Task 2.3** — `ROWS FROM()` with multiple functions | 1–2 days | ✅ **DONE** — `rewrite_rows_from()` auto-rewrite pass |
| 17 | **Part 2 Task 2.4** — LATERAL with RIGHT/FULL JOIN: error message clarification | 0.5 day | ✅ **DONE** — error messages updated to explain PostgreSQL-level constraint |

**Completion gate:** `just test-all` green + new E2E tests from each task passing.

### Stage 3 Phase 2 — Complete

All Phase 2 items are implemented, including the previously deferred Task 2.3
(ROWS FROM). The `rewrite_rows_from()` pass detects multi-function `ROWS FROM()`
nodes and rewrites to multi-arg `unnest()` (all-unnest case) or an ordinal-based
LEFT JOIN LATERAL chain (general case).

---

## Stage 4 — P1 Remainder + Trigger-Level Optimisations

These can proceed in parallel if bandwidth allows (different files: EC-16
touches `src/refresh.rs`; Part 2 Phase 3 touches `src/cdc.rs`).

### EC-16 (PLAN_EDGE_CASES P1 remainder)

| # | Item | Effort | Status |
|---|------|--------|--------|
| 18 | **EC-16** — `pg_proc` hash polling for undetected ALTER FUNCTION changes | 2 days | ✅ Done — `function_hashes` TEXT column on `pgt_stream_tables` (migration 0.2.1→0.2.2); `check_proc_hashes_changed()` in `refresh.rs` computes `md5(prosrc)` map from `pg_proc` and drives `mark_for_reinitialize` on mismatch |

### Part 2 Phase 3 — Trigger-Level Optimisations

| # | Item | Effort | Status |
|---|------|--------|--------|
| 19 | **Part 2 Task 3.1** — Column-level change detection for UPDATE: `changed_cols` bitmask in buffer | 3–4 days | ✅ Done — `changed_cols BIGINT` added to all new change buffer tables; `build_changed_cols_bitmask_expr()` generates per-column `IS DISTINCT FROM` bitmask; `create_change_trigger()` + `rebuild_cdc_trigger_function()` updated; `sync_change_buffer_columns()` preserves column; `alter_change_buffer_add_columns()` migrates existing buffers; all column values still written (scan unchanged) |
| 20 | **Part 2 Task 3.2** — Incremental TRUNCATE: negation delta for simple single-source stream tables | 2–3 days | ✅ Done — `execute_incremental_truncate_delete()` fast-path: single-source ST with no post-TRUNCATE rows in window → `DELETE FROM stream_table` directly, skipping full defining-query re-execution; falls back to `execute_full_refresh()` for multi-source or post-TRUNCATE-insert cases |
| 21 | **Part 2 Task 3.3** — Buffer table partitioning by LSN range: `pg_trickle.buffer_partitioning` GUC | 3–4 days | ✅ Done — `pg_trickle.buffer_partitioning` GUC (off/on/auto); `PARTITION BY RANGE (lsn)` + default partition; `detach_consumed_partitions()` O(1) cleanup; `should_auto_partition()` checks schedule ≥ 30s; cleanup paths in `drain_pending_cleanups()` and `cleanup_change_buffers_by_frontier()` updated |
| 22 | **Part 2 Task 3.4** — Skip-unchanged-column scanning in delta SQL | 1–2 days | ✅ Done — `prune_scan_columns()` demand-propagation pass in parser; collects `Expr::ColumnRef` from full tree; prunes `Scan.columns` to referenced + PK columns; bails out safely on `Star`/`Raw`/`LateralFunction`/`LateralSubquery` nodes; CTE bodies also pruned; 8 unit tests |
| 23 | **Part 2 Task 3.5** — Online ADD COLUMN without full reinit | 2–3 days | ✅ Done — `SchemaChangeKind::AddColumnOnly`; `alter_change_buffer_add_columns()` in `cdc.rs` extends buffer + rebuilds trigger + refreshes snapshot in-place |

**Note on ordering within Phase 3:** Task 3.1 must land before 3.4 (3.4
depends on the `changed_cols` bitmask). Tasks 3.2, 3.3, and 3.5 are
independent of each other.

**Completion gate:** `just test-all` green + buffer-size benchmarks showing
≥ 30% UPDATE buffer reduction for a 10-column table.

---

## Stage 5 — Remaining Aggregate Coverage (Part 2 Phase 4)

Pure additions. Independent of everything above.

| # | Item | Effort | Status |
|---|------|--------|--------|
| 24 | **Part 2 Task 4.1** — Regression aggregates: `CORR`, `COVAR_*`, `REGR_*` via group-rescan | 2–3 days | ✅ Done — already fully wired in prior session (`AggFunc::Corr/CovarPop/RegrAvgx` etc. + match arms + `is_group_rescan`) |
| 25 | **Part 2 Task 4.2** — Hypothetical-set aggregates: `RANK()`, `DENSE_RANK()`, `PERCENT_RANK()`, `CUME_DIST()` WITHIN GROUP | 1–2 days | ✅ Done — `HypRank/HypDenseRank/HypPercentRank/HypCumeDist` variants; `is_ordered_set` updated; match arm wired |
| 26 | **Part 2 Task 4.3** — `XMLAGG` via group-rescan | 0.5 day | ✅ Done — `AggFunc::XmlAgg`; `sql_name()` → `"XMLAGG"`; `is_group_rescan()` true; match arm wired |

**Completion gate:** `just test-all` green + 36+ aggregate functions confirmed in unit tests.

---

## Stage 6 — IMMEDIATE Mode Parity (Part 2 Phase 5)

Highest-risk work due to transaction and stack-depth interactions. Leave
until the engine is stable post-Stage 5.

| # | Item | Effort | Status |
|---|------|--------|--------|
| 27 | **Part 2 Task 5.1 / EC-09** — Recursive CTEs in IMMEDIATE mode: validate semi-naive with `DeltaSource::TransitionTable`; add stack-depth guard | 2–3 days | ✅ Done — `check_for_delete_changes()` extended for `TransitionTable` (OLD transition table row check); `generate_change_buffer_from()` uses `__pgt_newtable_<oid>` in IMMEDIATE context; `PGS_IVM_RECURSIVE_MAX_DEPTH` GUC (default 100) injects `__pgt_depth` counter + guard into semi-naive SQL via `try_generate_propagation_with_depth()`; `build_semi_naive_result()` helper extracts shared logic; warning updated to reference GUC |
| 28 | **Part 2 Task 5.2 / EC-09** — TopK in IMMEDIATE mode: statement-level micro-refresh; `ivm_topk_max_limit` GUC | 2–3 days | ✅ Done — `apply_topk_micro_refresh()` in ivm.rs; `PGS_IVM_TOPK_MAX_LIMIT` GUC (default 1000); threshold check in api.rs |

**Completion gate:** `just test-all` green + dedicated IMMEDIATE + recursive CTE E2E test passing.

**Stage 6 is COMPLETE.** Both IMMEDIATE mode parity items are fully implemented:
- Task 5.1 (fully landed): `check_for_delete_changes()` inspects OLD transition
  table rows (not change buffer) in IMMEDIATE mode to correctly select DRed vs
  semi-naive. `generate_change_buffer_from()` reads from `__pgt_newtable_<oid>`
  (same schema as source table) instead of change buffer INSERT rows. Depth guard
  GUC `pg_trickle.ivm_recursive_max_depth` (default 100) injects `__pgt_depth`
  counter into seeds and a `WHERE depth < max` guard into propagation SQL for
  IMMEDIATE mode via `try_generate_propagation_with_depth()`.  Logic extracted into
  `build_semi_naive_result()` helper shared by both semi-naive and DRed paths.
- Task 5.2: `apply_topk_micro_refresh()` added to `ivm.rs` — materializes top-K into
  a temp table, then applies DELETE + INSERT ON CONFLICT to update the stream table.
  Bounded by `pg_trickle.ivm_topk_max_limit` GUC (default 1000); queries above the
  threshold are rejected at creation/alter time.

---

## Stage 7 — P2 Usability Gaps + P3 Documentation Sweep

| # | Item | Effort | Status |
|---|------|--------|--------|
| 29 | **EC-05** — Foreign table error message improvement (short-term) | 0.5 day | ✅ Done — error message now suggests FULL mode + `postgres_fdw`/`IMPORT FOREIGN SCHEMA` |
| 29b | **EC-05** — Foreign table polling-based change detection (medium-term) | 2–3 days | ✅ Done — `pg_trickle.foreign_table_polling` GUC; `setup_foreign_table_polling()` creates change buffer + snapshot table; `poll_foreign_table_changes()` uses EXCEPT ALL for insert/delete deltas; snapshot refreshed each cycle; cleanup drops snapshot table |
| 30 | **EC-02** — `pg_trickle.max_grouping_set_branches` GUC | 0.5 day | ✅ Done — GUC in config.rs (default 64, range 1–65536); parser.rs uses dynamic lookup |
| 31 | **EC-20** — Post-restart CDC TRANSITIONING health check | 1 day | ✅ Done — `check_cdc_transition_health()` in scheduler.rs; detects missing replication slots; rolls back to TRIGGER mode |
| 32 | **EC-28** — PgBouncer configuration documentation | 0.5 day | ✅ Done (pre-existing FAQ section) |
| 33 | **EC-17** — DDL-during-refresh documentation clarification | 0.5 day | ✅ Done — FAQ section added explaining ShareLock/AccessExclusiveLock interaction |
| 34 | **EC-21/22/23** — Replication / standby limitations documentation sweep | 0.5 day | ✅ Done — FAQ section with limitations table + guidance |
| 35 | All remaining P3 items | — | See below |

**Stage 7 is COMPLETE.** All targeted items are implemented:
- EC-02: Configurable grouping set branch limit via GUC.
- EC-05: Error message improved + polling CDC implemented (`pg_trickle.foreign_table_polling` GUC).
- EC-20: Post-restart health check validates CDC transition state.
- EC-17, EC-21/22/23, EC-28: Documentation added to FAQ and CONFIGURATION.md.
- Three new GUCs documented: `max_grouping_set_branches`, `ivm_topk_max_limit`,
  `buffer_alert_threshold`.

---

## Summary Table

| Stage | Source | Items | Status |
|-------|--------|-------|--------|
| 1 — P0 Correctness | PLAN_EDGE_CASES | 3 | ✅ COMPLETE |
| 2 — P1 Safety | PLAN_EDGE_CASES | 10 | ✅ COMPLETE |
| 3 — SQL Coverage | Part 2 Ph 1–2 | 7 | ✅ COMPLETE |
| 4 — P1 Remainder + Triggers | EC-16 + Part 2 Ph 3 | 6 | ✅ COMPLETE |
| 5 — Aggregates | Part 2 Ph 4 | 3 | ✅ COMPLETE |
| 6 — IMMEDIATE Parity | Part 2 Ph 5 | 2 | ✅ COMPLETE |
| 7 — Usability + Docs | PLAN_EDGE_CASES P2/P3 | 7+ | ✅ COMPLETE |

---

## Prioritized Remaining Work

All seven stages are now complete.  The one item that needed a full correctness
pass — IMMEDIATE mode + `WITH RECURSIVE` — has been fully resolved:

**Task 5.1 landing (March 2026):**
- `check_for_delete_changes()` now inspects OLD transition table rows in IMMEDIATE
  mode, correctly routing DELETE/UPDATE triggers to the DRed strategy.
- `generate_change_buffer_from()` uses `__pgt_newtable_<oid>` (same schema as source)
  in IMMEDIATE mode, fixing the "seed from existing storage" step.
- `PGS_IVM_RECURSIVE_MAX_DEPTH` GUC (default 100) adds a depth counter and guard to
  the generated semi-naive SQL via `try_generate_propagation_with_depth()`.
- `build_semi_naive_result()` helper DRYs up both the semi-naive and DRed insert
  propagation paths.
- E2E tests added: INSERT-only (semi-naive), DELETE (DRed), and depth guard.
- Docs updated: SQL_REFERENCE.md, README.md, ROADMAP.md.

**Task 5.2 landing (March 2026):**
- `apply_topk_micro_refresh()` in `ivm.rs` materializes top-K into temp table,
  DELETEs departed rows, INSERTs/UPDATEs new rows via INSERT ON CONFLICT.
- `PGS_IVM_TOPK_MAX_LIMIT` GUC (default 1000) — queries exceeding this threshold
  are rejected at stream table creation time with a clear error message.
- Threshold validation added to `validate_and_parse_query()` in api.rs.
- 10 E2E tests added to `e2e_topk_tests.rs`: basic creation, INSERT enters top-N,
  INSERT below threshold (no change), DELETE expands window, UPDATE changes ranking,
  multiple DML in transaction, aggregate TopK, OFFSET TopK, GUC threshold rejection,
  mode switch from DIFFERENTIAL to IMMEDIATE.

### Prioritized Remaining Work (v0.2.2)

All IMMEDIATE mode parity items are complete. The remaining v0.2.2 roadmap
items, in priority order:

1. **Edge Case Hardening**
   - EC1: `max_grouping_set_branches` GUC — cap CUBE/ROLLUP branch explosion (4h)
   - EC2: Post-restart CDC TRANSITIONING health check (1d)
   - EC3: Foreign table polling-based change detection (2–3d)

2. **Documentation Sweep**
   - DS1: DDL-during-refresh behaviour documentation (2h)
   - DS2: Replication/standby limitations (3h)
   - DS3: PgBouncer configuration guide (2h)

3. **WAL CDC Hardening**
   - W1: WAL mode E2E test suite (8–12h)
   - W2: WAL→trigger automatic fallback hardening (4–6h)
   - W3: Promote `cdc_mode = 'auto'` to recommended (~1h)

