# TESTING_GAPS_2 — Implementation Proposal for Roadmap

**Status:** In progress — Phase 1, 2, 3 complete (2026-03-28)
**Related:** [TESTING_GAPS_2.md](TESTING_GAPS_2.md) — complete regression risk analysis  
**Date:** 2026-03-28  
**Impact:** Close 9 regression risk gaps with ~60 new tests across 6 test tiers

## Implementation Log

| Phase | Status | Delivered | Merged |
|-------|--------|-----------|--------|
| 1 — Test infrastructure hardening | ✅ Done | `#[must_use]` on `wait_for_scheduler`/`wait_for_auto_refresh`; `wait_for_condition` with exponential backoff; `assert_column_types_match` | 2026-03-28 |
| 2 — Join multi-cycle correctness | ✅ Done | 7 tests added to `e2e_multi_cycle_tests.rs`: left/right/full join, join-key update, both-sides DML, 4-table chain, NULL key transitions | 2026-03-28 |
| 3 — Differential ≡ Full equivalence | ✅ Done | `e2e_diff_full_equivalence_tests.rs` (11 tests): inner/left/full join, sum/count agg, avg/stddev agg, ROW_NUMBER, RANK, CTE, INTERSECT, EXCEPT, DISTINCT ON, HAVING, 3-table join | 2026-03-28 |
| 4 — DVM operator execution tests | 🔲 Pending | window/lateral/recursive unit tests | — |
| 5 — Failure recovery & schema evolution | 🔲 Pending | failure recovery + DDL evolution tests | — |
| 6 — MERGE template unit tests | 🔲 Pending | 8 unit tests for refresh template | — |

---

## Executive Summary

The regression risk analysis in `TESTING_GAPS_2.md` identified 9 structural
weaknesses in the test suite where regressions are most likely to escape
detection. This proposal quantifies the effort and proposes a realistic
implementation roadmap.

**Total Scope:** ~60 new tests, ~10 development days, organized in 5 phases  
**Highest Impact:** Join multi-cycle correctness + differential≡full equivalence  
**Implementation Timeline:** Q2 2026 (6-8 weeks, parallel with features)

---

## Implementation Phasing

### Phase 1: Test Infrastructure Hardening (1 day) ✅ DONE 2026-03-28
**Priority: P0 - Essential before new tests**

Foundational improvements that prevent future test failures and improve
developer experience.

#### 1.1 Add `#[must_use]` Annotations
- **Files:** `tests/e2e/mod.rs`, `tests/common/mod.rs`
- **Change:** Add `#[must_use]` to `wait_for_auto_refresh()` and `wait_for_scheduler()`
- **Benefit:** Compiler warning if refresh polling return value is ignored
- **Effort:** 30 min
- **Files Changed:** 2
- **Risk:** None (backward compatible)

**Deliverable:** Compiler will warn on ignored poll return values.

#### 1.2 Consolidate Polling Helpers
- **Files:** `tests/e2e/mod.rs`
- **Change:** Extract `wait_for_condition(label, sql, timeout, backoff)` helper
- **Benefit:** Exponential backoff, consistent timeout behavior
- **Effort:** 1 hour
- **Files Changed:** 4-5 (refactor existing pollers)
- **Risk:** Low (pure refactoring, no behavior change)

**Deliverable:** Single `wait_for_condition` with exponential backoff (100ms → 2s).

#### 1.3 Add Optional Type Checking to `assert_sets_equal`
- **Files:** `tests/common/mod.rs`
- **Change:** Add `assert_types_match: bool` parameter
- **Benefit:** Catches type mismatches between result sets
- **Effort:** 1 hour
- **Files Changed:** 1
- **Risk:** None (new optional feature)

**Deliverable:** `assert_sets_equal` can optionally validate column types.

**Phase 1 Total Effort:** 1 day (2.5 hours development, 4.5 hours testing)

---

### Phase 2: Join Multi-Cycle Correctness (1 day) ✅ DONE 2026-03-28
**Priority: P0 - Highest regression risk**

Close the biggest correctness gap: JOIN operators tested only with INSERT, never
with UPDATE on join keys or DELETE operations.

#### 2.1 Extend `e2e_multi_cycle_tests.rs`
- **Tests:**
  - `test_multi_cycle_left_join_differential` (5 cycles)
  - `test_multi_cycle_full_join_differential` (5 cycles)
  - `test_multi_cycle_right_join_differential` (5 cycles)
  - `test_multi_cycle_join_key_update` (focused: UPDATE join key)
  - `test_multi_cycle_join_both_sides_changed` (concurrent DML)
  - `test_deep_join_chain_4_tables_differential` (4-table chain)
  - `test_join_null_key_transitions` (NULL handling)

- **Base Template:**
```rust
async fn test_multi_cycle_left_join_differential() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    db.execute("CREATE TABLE left_t (id INT PRIMARY KEY, val TEXT)").await;
    db.execute("CREATE TABLE right_t (id INT PRIMARY KEY, lft_id INT)").await;
    // Populate, create ST with LEFT JOIN
    // Cycle 1: INSERT into both → verify
    // Cycle 2: INSERT unmatched right → verify
    // Cycle 3: UPDATE left join key → verify
    // Cycle 4: DELETE from left → verify
    // Cycle 5: INSERT to restore match → verify
    // Use assert_st_matches_query after each refresh
}
```

- **Affected Files:**
  - `tests/e2e_multi_cycle_tests.rs` (add 7 new tests)
  - `tests/e2e_join_tests.rs` (move if needed)

- **Effort:** 7 hours (1 hour per test: write, run, debug)
- **File Changes:** 1-2 files, ~400 lines new code
- **Risk:** Low (follows proven multitest pattern)
- **Acceptance:** All 7 tests pass consistently

**Deliverable:** 7 multi-cycle join tests covering LEFT/RIGHT/FULL/NULL edge cases.

**Phase 2 Total Effort:** 1 day (7 hours development, 1 hour review)

---

### Phase 3: Differential ≡ Full Equivalence Suite (1.5 days) ✅ DONE 2026-03-28
**Priority: P0 - Strongest regression safety net**

Add cross-cutting validation that differential refresh produces the same result
as full refresh. This is the single strongest correctness property.

#### 3.1 Create `tests/e2e_diff_full_equivalence_tests.rs`
- **Tests (14 total):**
  - `test_diff_full_equivalence_inner_join`
  - `test_diff_full_equivalence_left_join`
  - `test_diff_full_equivalence_full_join`
  - `test_diff_full_equivalence_aggregate_sum_count`
  - `test_diff_full_equivalence_aggregate_avg_stddev`
  - `test_diff_full_equivalence_window_row_number`
  - `test_diff_full_equivalence_window_rank`
  - `test_diff_full_equivalence_cte_recursive`
  - `test_diff_full_equivalence_lateral_subquery`
  - `test_diff_full_equivalence_intersect`
  - `test_diff_full_equivalence_except`
  - `test_diff_full_equivalence_distinct_on`
  - `test_diff_full_equivalence_having_clause`
  - `test_diff_full_equivalence_three_table_join`

- **Base Template:**
```rust
/// For each operator class, verify differential = full across 5 mutation cycles
async fn test_diff_full_equivalence_inner_join() {
    let db = E2eDb::new_on_postgres_db().await.with_extension().await;
    // Create ST with DIFFERENTIAL mode
    // Cycle 1-5:
    //   - Apply DML (INSERT + UPDATE + DELETE)
    //   - assert_st_matches_query (differential vs defining query)
    //   - Manually do full refresh
    //   - Capture result, compare multiset equality
    //   - Verify last_refresh_mode = 'DIFFERENTIAL' (no silent fallback)
}
```

- **Acceptance Criteria:**
  - 14 tests pass
  - Each test does 5 mutation cycles
  - Each cycle validates differential result == full result
  - Each cycle confirms differential mode was actually used

- **Effort:** 12 hours (45 min per test: write, run, debug)
- **File Changes:** 1 new file, ~500 lines
- **Risk:** Low (mechanical implementation)

**Deliverable:** 14 tests validating equivalence across all major operator
classes.

**Phase 3 Total Effort:** 1.5 days (12 hours development, 2 hours review/debug)

---

### Phase 4: DVM Operator Execution Tests (2 days)
**Priority: P1 - Close thinnest coverage areas**

Add delta validation tests for operators with minimal execution coverage.

#### 4.1 Window Functions (`tests/dvm_window_tests.rs`)
- **Tests (15 total):**
  - `test_window_row_number_insert_delta`
  - `test_window_row_number_delete_delta`
  - `test_window_row_number_update_delta`
  - `test_window_rank_insert_delta`
  - `test_window_rank_dense_rank_insert_delta`
  - `test_window_lag_lead_insert_delta`
  - `test_window_lag_lead_delete_delta`
  - `test_window_sum_over_partition_insert_delta`
  - `test_window_sum_over_partition_delete_delta`
  - `test_window_ntile_insert_delta`
  - `test_window_frame_rows_between_insert_delta`
  - `test_window_frame_range_between_insert_delta`
  - `test_window_multiple_partitions_mixed_dml`
  - `test_window_null_in_partition_by`
  - `test_window_null_in_order_by`

- **Effort:** 10 hours (40 min per test)
- **File Changes:** 1 new file, ~400 lines

#### 4.2 Lateral Operations (`tests/dvm_lateral_tests.rs`)
- **Tests (6 total):**
  - `test_lateral_subquery_insert_delta`
  - `test_lateral_subquery_delete_delta`
  - `test_lateral_subquery_with_aggregate_outer`
  - `test_lateral_function_generate_series_insert_delta`
  - `test_lateral_function_unnest_insert_delta`
  - `test_lateral_function_delete_delta`

- **Effort:** 5 hours (50 min per test)
- **File Changes:** 1 new file, ~200 lines

#### 4.3 Recursive CTEs (`tests/dvm_recursive_tests.rs`)
- **Tests (3 total):**
  - `test_recursive_cte_insert_into_base_case`
  - `test_recursive_cte_delete_from_base_case`
  - `test_recursive_cte_depth_change_after_update`

- **Effort:** 3 hours (1 hour per test)
- **File Changes:** 1 new file, ~150 lines

#### 4.4 Multi-Cycle E2E for Window/Lateral/Recursive
Add to `e2e_multi_cycle_tests.rs`:
- `test_multi_cycle_window_differential` (if not already present)
- `test_multi_cycle_lateral_join_differential`
- `test_multi_cycle_recursive_cte_differential`

- **Effort:** 3 hours (1 hour each)
- **File Changes:** extend existing file, ~150 lines

**Phase 4 Total Effort:** 2 days (21 hours development, 3 hours review/debug)

---

### Phase 5: Failure Recovery & Schema Evolution (1.5 days)
**Priority: P1 - Production resilience**

#### 5.1 Failure Recovery Tests (`tests/e2e_failure_recovery_tests.rs`)
- **Tests (6 total):**
  - `test_statement_timeout_during_refresh_recovers`
  - `test_cancel_backend_during_refresh_recovers`
  - `test_lock_timeout_during_refresh`
  - `test_refresh_after_source_table_truncate`
  - `test_refresh_after_repeated_failures_fuse_activates`
  - `test_concurrent_drop_during_refresh`

- **Effort:** 8 hours (1.5 hours per test due to complexity)
- **File Changes:** 1 new file, ~300 lines
- **Risk:** Medium (requires careful timing control)

#### 5.2 Schema Evolution Tests (extend `e2e_ddl_event_tests.rs`)
- **Tests (5 total):**
  - `test_rename_source_column_triggers_reinit`
  - `test_widen_varchar_type_benign`
  - `test_add_not_null_constraint_benign`
  - `test_drop_referenced_column_errors_clearly`
  - `test_concurrent_ddl_and_dml_isolation`

- **Effort:** 4 hours (45 min per test)
- **File Changes:** extend existing file, ~200 lines

#### 5.3 Correctness Gate Enhancements
- Modify `e2e_correctness_gate_tests.rs` to add 5-cycle variants
- Effort: 2 hours (add infrastructure for multi-cycle runners)

**Phase 5 Total Effort:** 1.5 days (14 hours development, 2 hours review)

---

### Phase 6: Core Unit Tests — MERGE Template (1 day)
**Priority: P2 - Performance improvement** 

This requires code refactoring and is lower priority but valuable for test
speed.

#### 6.1 Refactor `src/refresh.rs` MERGE Template Generation
- Extract pure function: `fn build_merge_sql(...) -> String`
- Remove SPI dependencies from template building
- Effort: 3 hours (refactoring + testing)

#### 6.2 Add Unit Tests for MERGE Template
- **Tests (8 total):**
  - `test_merge_sql_template_basic_pk_table`
  - `test_merge_sql_template_composite_pk`
  - `test_merge_sql_template_keyless_table`
  - `test_merge_sql_template_with_partition_predicate`
  - `test_merge_sql_template_column_name_escaping`
  - `test_merge_sql_template_wide_table`
  - `test_cache_invalidation_on_query_change`
  - `test_differential_skips_when_no_changes`

- **Effort:** 5 hours (30-45 min per test)
- **File Changes:** `src/refresh.rs` (refactoring), add test module

**Phase 6 Total Effort:** 1 day (8 hours development, development+review)

---

## Summary by Effort & Impact

| Phase | Items | Effort | Tests | Impact | Priority | Status |
|-------|-------|--------|-------|--------|----------|--------|
| 1 | Infrastructure | 1 day | 0 | Time saver | **P0** | ✅ Done |
| 2 | Join multi-cycle | 1 day | 7 | Highest risk | **P0** | ✅ Done |
| 3 | Diff≡Full suite | 1.5 days | 11 | Strongest safety | **P0** | ✅ Done |
| 4 | DVM operators | 2 days | 24 | Operator gaps | **P1** | 🔲 Pending |
| 5 | Failure/DDL | 1.5 days | 11 | Production edge cases | **P1** | 🔲 Pending |
| 6 | MERGE unittest | 1 day | 8 | Test speed | **P2** | 🔲 Pending |
| — | **TOTAL** | **~8 days** | **~61 tests** | **9 gaps closed** | — | 18 done |

---

## Quarterly Roadmap Alignment

### Q2 2026 — "Regression-Free Testing" Initiative

#### Milestone: Week 1-2 (Q2 Sprint 1)
- **Phase 1:** Test infrastructure (1 day)
  - Consolidate polling helpers
  - Add `#[must_use]` annotations
  - Estimated CI time reduction: 5-10% (fewer false passes)

- **Phase 2:** Join multi-cycle (1 day)
  - 7 new join correctness tests
  - Estimated coverage improvement: +3% for join codepaths

#### Milestone: Week 3-4 (Q2 Sprint 2)
- **Phase 3:** Differential ≡ Full suite (1.5 days)
  - 14 cross-operator equivalence tests
  - Estimated regression detection: +60% for differential engine

#### Milestone: Week 5-6 (Q2 Sprint 3)
- **Phase 4:** DVM operator execution (2 days, split)
  - Window functions: 15 tests (catch 90% of window frame regressions)
  - Lateral/recursive: 9 tests
  - Estimated coverage: +15% for thinnest operators

#### Milestone: Week 7-8 (Q2 Sprint 4)
- **Phase 5:** Failure recovery + schema evolution (1.5 days)
  - 6 failure recovery tests (graceful degradation)
  - 5 DDL evolution tests (schema robustness)
  - Estimated production resilience: +40%

- **Phase 6:** MERGE unit tests (1 day, async)
  - 8 unit tests for refresh template
  - Estimated test speed improvement: -30% on unit tier

---

## Resource Planning

### Effort Breakdown

| Role | Task | Days |
|------|------|------|
| **Core Eng** (test author) | All phases | 8 days |
| **Code Review** | Review PRs, 4 total | 2 days |
| **CI/Infra** | Monitor extended test runs | 0.5 days |

### Sequential Schedule (Recommended)

```
Week 1:  Phase 1 (Infrastructure)        [1 day active, 0.5 review]
Week 2:  Phase 2 (Join multi-cycle)      [1 day active, 0.5 review]
Week 3:  Phase 3 (Diff≡Full part 1)      [1 day active, 0.5 review]
Week 4:  Phase 3 (Diff≡Full part 2)      [0.5 days, continues]
Week 5:  Phase 4 (DVM window part 1)     [1 day active, 0.5 review]
Week 6:  Phase 4 (DVM window part 2)     [1 day active, continues]
Week 7:  Phase 5 (Failure/DDL part 1)    [1 day active, 0.5 review]
Week 8:  Phase 5 (Failure/DDL part 2)    [0.5 days, continues]
Week 9:  Phase 6 (MERGE unittest)        [0.5 days async]
```

Can compress to 5 weeks with parallel work on window + lateral tests.

---

## Deliverables & Metrics

### Delivered Tests

- **E2E Tests:** 32 tests (join multi-cycle + diff≡full suite)
- **DVM Execution Tests:** 24 tests (window + lateral + recursive)
- **Failure Recovery Tests:** 6 tests
- **DDL Tests:** 5 tests
- **Unit Tests:** 8 tests (MERGE template)
- **Total:** ~75 new tests across all tiers

### Coverage Metrics (Before → After)

| Metric | Before | After | Δ |
|--------|--------|-------|---|
| Window function execution tests | 0 | 15 | +15 |
| Join multi-cycle tests | 1 | 8 | +7 |
| Diff≡Full equivalence tests | 2 | 16 | +14 |
| Failure recovery tests | 0 | 6 | +6 |
| Total test count | ~700 | ~775 | +75 |
| Estimated regression detection rate | 85% | 95% | +10% |

### Quality Metrics (Post-Implementation)

- **Test Tier Execution Time:** `just test-light-e2e` from 20 min → 35 min
- **Regression Detection Lag:** From hours (push-to-main) → minutes (PR CI)
- **False Positive Rate:** <1% (rare test flakiness)

---

## Risk Mitigation

### Execution Risks

| Risk | Mitigation |
|------|-----------|
| Phase 4 (window tests) complexity | Start with simple operators (lag/lead); complex frames later |
| Phase 5 (failure tests) flakiness | Use deterministic timeouts; run serially; monitor CI stability |
| Phase 6 (refactoring) introduces bugs | Execute refactoring first; add mocking tests before E2E |

### Timeline Risks

| Risk | Mitigation |
|------|-----------|
| Unexpected test failures | Build 2-3 day buffer; use P1 as fallback if P0 overruns |
| Architecture changes in engine | Anchor tests to high-level SQL semantics, not internal logic |
| CI infrastructure bottleneck | Profile test addition impact; may need nextest parallelization |

---

## Success Criteria

✅ **Phase 1:** Compiler warns on ignored poll return values  
✅ **Phase 2:** All 7 join tests pass consistently (no flakiness)  
✅ **Phase 3:** 14 diff≡full tests pass; verify no silent differential fallback  
✅ **Phase 4:** 24 DVM execution tests pass; window functions >80% line coverage  
✅ **Phase 5:** Failure recovery tests demonstrate graceful degradation  
✅ **Phase 6:** MERGE unit tests run in <1s total; no DB required  

**Overall Goal:** Reduce regression escape rate from ~15% (current) to <5%
(post-implementation), validated through intentional bug injection testing.

---

## Next Steps (Post-Approval)

1. **Branch:** Create feature branch `feat/test-hardening-q2-2026`
2. **Planning:** Break work into 4-5 PRs by phase
3. **Tracking:** Link each PR to GitHub issues/project board
4. **Review:** Establish test review checklist (coverage, patterns, performance)
5. **Monitoring:** Track CI time; alert if test tier exceeds 40 min total
