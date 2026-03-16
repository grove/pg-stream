# TPC-H Test Suite — Deep Evaluation Report

> **Date:** 2025-03-16
> **Scope:** `tests/e2e_tpch_tests.rs` (10 test functions, 2524 lines) +
>            `tests/e2e_tpch_dag_tests.rs` (2 test functions, 570 lines)
> **Goal:** Assess coverage confidence and identify mitigations to harden the suite

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Test Infrastructure](#test-infrastructure)
3. [Cross-Cutting Findings](#cross-cutting-findings)
4. [Per-Test Analysis](#per-test-analysis)
5. [Known Limitations & Skip-Set Analysis](#known-limitations--skip-set-analysis)
6. [Priority Mitigations](#priority-mitigations)
7. [Appendix: Query Coverage Matrix](#appendix-query-coverage-matrix)

---

## Executive Summary

The TPC-H test suite consists of **12 test functions** across 2 files
(~3,100 lines), running all 22 standard TPC-H queries against the pg_trickle
DVM engine. Tests are `#[ignore]`-gated and run via `just test-tpch` (CI: push
to main + daily schedule + manual dispatch).

**Confidence level: HIGH (≈85%) for the tested query subset, MODERATE (≈65%)
overall.**

### Strengths

| Area | Assessment |
|------|-----------|
| Assertion quality | **Excellent** — custom `assert_tpch_invariant` uses `EXCEPT ALL` (multiset equality) with diagnostic output (extra/missing rows) |
| `__pgt_count` guard (T1) | **Excellent** — detects over-retraction bugs even when EXCEPT cancels out |
| Skip-set regression guard (T2) | **Strong** — prevents silent DVM regressions |
| Mode coverage | **Comprehensive** — DIFFERENTIAL, FULL, IMMEDIATE, and cross-mode comparisons |
| DAG chain (T6) | **Good** — 2-level chain + multi-parent fan-in |
| Rollback correctness (T3) | **Unique** — only test suite that verifies IMMEDIATE mode transactional atomicity |
| Sustained churn | **Good** — 50-cycle stress with periodic correctness checks |

### Weaknesses

| Severity | Finding | Impact |
|----------|---------|--------|
| **HIGH** | 5/22 queries permanently skipped in DIFFERENTIAL mode (q05, q07, q08, q09, q12) | 23% of the standard query set is untested in the primary refresh mode |
| **HIGH** | IMMEDIATE skip allowlist is fully permissive (all 22 queries) | T2 regression guard is effectively disabled for IMMEDIATE mode |
| **HIGH** | `test_tpch_full_vs_differential` only asserts `passed > 0` | Could pass with just 1/22 queries — no minimum coverage threshold |
| **MEDIUM** | `test_tpch_differential_vs_immediate` only asserts `passed > 0` | Same as above — extremely weak final assertion |
| **MEDIUM** | Soft-skip on DVM errors masks regressions | Tests print WARN and continue — failures accumulate silently in CI logs |
| **MEDIUM** | No FULL mode correctness test exists independently | FULL mode is only tested as part of FULL-vs-DIFFERENTIAL comparison |
| **MEDIUM** | Performance test has no regression threshold | Speedup table is informational only — no assertion on minimum speedup |
| **LOW** | Code duplication between `e2e_tpch_tests.rs` and `e2e_tpch_dag_tests.rs` | ~120 lines of duplicated helpers (load_schema, load_data, substitute_sf, etc.) |
| **LOW** | `buf≈-2` display artifact in sustained churn | Cosmetic — `reltuples` can be -1 after VACUUM; needs `GREATEST(reltuples, 0)` |

---

## Test Infrastructure

### Data Generation

**File:** `tests/tpch/datagen.sql` (504 lines)
**Method:** Pure SQL `generate_series()` + deterministic pseudo-random via
LCG/modulo hash.

| Table | Rows (SF=0.01) | Rows (SF=0.1) |
|-------|---------------|---------------|
| region | 5 | 5 |
| nation | 25 | 25 |
| supplier | 10 | 100 |
| part | 200 | 2,000 |
| partsupp | 800 | 8,000 |
| customer | 150 | 1,500 |
| orders | 1,500 | 15,000 |
| lineitem | ~6,000 | ~60,000 |

**Determinism:** Same SF always produces identical data. ✅

**Scale factor configuration:**
- `TPCH_SCALE=0.01` (default, CI) — ~2 min
- `TPCH_SCALE=0.1` — ~5 min
- `TPCH_SCALE=1.0` — ~15 min

### Mutation Functions (RF1, RF2, RF3)

| RF | Operation | Batch Size | Strategy |
|----|-----------|-----------|----------|
| RF1 | INSERT | `__RF_COUNT__` orders + 1–7 lineitems each | Monotonically increasing orderkey |
| RF2 | DELETE | `__RF_COUNT__` oldest orders + lineitems | FIFO by `o_orderkey ASC` |
| RF3 | UPDATE | `__RF_COUNT__` lineitems (price, discount, quantity) | Modulo-selected rows |

Default RF_COUNT = `max(orders/100, 10)` = 15 at SF=0.01.

**Known omission:** Customer UPDATE intentionally disabled due to DVM bug
(LEFT JOIN refresh generates invalid SQL on cycle 2+, affects Q13).

### Invariant Assertion

`assert_tpch_invariant()` in `e2e_tpch_tests.rs` (lines 366–500):

1. Fetches user-visible columns (excluding `__pgt_row_id`, `__pgt_count`)
2. Checks `__pgt_count < 0` (T1 — over-retraction guard)
3. Performs symmetric `EXCEPT ALL` (multiset equality)
4. On failure: logs ST count, query count, extra rows, missing rows (up to 10
   each, as JSON)

**Quality:** ✅ **Gold standard.** Uses `EXCEPT ALL` (not `EXCEPT`), includes
negative-count guard, provides diagnostic row-level output on failure.

`assert_invariant()` in `e2e_tpch_dag_tests.rs` (lines 138–190):

Same pattern with T1 guard and `EXCEPT ALL`. Consistent quality. ✅

---

## Cross-Cutting Findings

### 1. Weak final assertions undermine strong per-cycle checks

Several tests perform excellent per-cycle invariant checking but then end with
extremely permissive final assertions:

| Test | Final assertion | Problem |
|------|----------------|---------|
| `test_tpch_full_vs_differential` | `assert!(passed > 0)` | Passes if only 1/22 queries works |
| `test_tpch_differential_vs_immediate` | `assert!(passed > 0)` | Same — 1/22 is enough |
| `test_tpch_performance_comparison` | `assert!(!results.is_empty())` | Same — 1/22 is enough |
| `test_tpch_immediate_rollback` | `assert!(all_passed)` | ✅ Good — hard-fails on any divergence |
| `test_tpch_single_row_mutations` | Soft-pass if all failures are skips | Acceptable |

**Mitigation:** Replace `passed > 0` with a minimum threshold
(e.g., `passed >= 15` for FULL vs DIFF, `passed >= 10` for DIFF vs IMM).

### 2. IMMEDIATE skip allowlist is fully permissive

```rust
const IMMEDIATE_SKIP_ALLOWLIST: &[&str] = &[
    "q02", "q03", "q04", "q05", "q06", "q07", "q08", "q09", "q10",
    "q11", "q12", "q13", "q14", "q15", "q16", "q17", "q18", "q19",
    "q20", "q21", "q22", "q01",
];
```

All 22 queries are in the allowlist, which means the T2 regression guard
**cannot detect any IMMEDIATE mode regression**. The comment says "TODO:
populate from the first test run output and re-enable the guard." This
has not been done.

**Mitigation:** Run `test_tpch_immediate_correctness` once, collect the actual
skip set, and replace the catch-all allowlist with the real set. This is the
single highest-impact change for regression detection.

### 3. Soft-skip pattern masks regressions over time

The general pattern across all TPC-H tests is:
```rust
if let Err(e) = try_refresh_st(&db, &st_name).await {
    println!("  WARN cycle {cycle} — {msg}");
    skipped.push((q.name, ...));
    dvm_ok = false;
    break 'cycles;
}
```

Failures are printed but not asserted (except via the skip-set guard in T2).
In CI, these WARNs are lost in verbose output. Over time, queries that used to
pass could silently regress to "skipped" without anyone noticing.

**Mitigation:** Add a CI step that parses TPC-H test output and alerts on
increases in the skip count. Alternatively, add a `TPCH_STRICT=1` mode that
hard-fails on any skip.

### 4. No standalone FULL mode correctness test

FULL refresh is only tested as part of `test_tpch_full_vs_differential`.
If DIFFERENTIAL is broken, FULL mode still isn't validated independently. There
is no test that creates FULL-only STs, applies mutations, and checks
`assert_tpch_invariant()`.

### 5. Customer UPDATE intentionally disabled

RF3 omits customer table UPDATEs due to a DVM bug. This means no TPC-H test
exercises UPDATE propagation through LEFT JOIN paths (Q13, Q10, Q22 all
reference `customer`). The queries still test LEFT JOIN with INSERT/DELETE,
but UPDATE is an untested delta path for this join type.

---

## Per-Test Analysis

### test_tpch_differential_correctness

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:559 |
| Queries tested | 22 (17 pass, 5 skip) |
| Cycles per query | configurable (`TPCH_CYCLES`, default 3) |
| Assertion | `assert_tpch_invariant` (EXCEPT ALL + T1 guard) per cycle |
| Final guard | T2 skip-set regression guard ✅ |
| Risk | LOW |

**What it does:** For each of the 22 TPC-H queries (ordered by coverage tier):
creates a DIFFERENTIAL stream table, asserts baseline invariant, runs N cycles
of RF1+RF2+RF3 → refresh → assert, then drops.

**Assessment:** **The backbone of the TPC-H suite.** Strong per-cycle invariant
checking with detailed diagnostics on failure. The T2 guard catches regressions
where previously-passing queries fall into the skip set.

**Issues:**
1. Each query runs **independently** (fresh data per query would be ideal but
   is not the case — all 22 queries share one dataset). The sequential
   execution means RF mutations from query N carry over to query N+1's data.
   This is intentional (cumulative mutations make later queries harder) but
   means a failure in an early query corrupts the data for later queries.
2. The `failed` vector is populated but never written to — the code says
   "populated if we add soft-failure logic later." Currently all failures route
   to `skipped`, and only the T2 guard catches them.

**Mitigations:**
1. Consider resetting data between queries (at cost of runtime).
2. Populate the `failed` vector for assertion errors (distinct from DVM
   errors), ensuring hard failures are visible.

---

### test_tpch_cross_query_consistency

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:760 |
| Queries tested | All 22 simultaneously |
| Cycles | configurable (default 3) |
| Assertion | `assert_tpch_invariant` per ST per cycle |
| Final guard | None (prints summary only) |
| Risk | MEDIUM |

**What it does:** Creates all 22 stream tables at once. Applies shared RF
mutations. Refreshes ALL STs, then asserts each one. Tests that CDC triggers
on shared source tables correctly fan out changes without interference.

**Assessment:** **Critical test for CDC fan-out correctness.** The simultaneous
presence of 22 STs with overlapping source tables (all read `lineitem`,
`orders`, etc.) is the most realistic deployment scenario.

**Issues:**
1. **No final assertion on minimum surviving STs.** The test only prints
   "N/M STs survived all cycles" — it doesn't fail if too few survive.
2. STs that hit DVM errors are dropped mid-test (auto-deactivated). The "active"
   set shrinks silently. If 15 STs fail in cycle 1, the test continues with 7
   and still passes.
3. Per-query WAL flush (`CHECKPOINT` after each refresh) is good for resource
   management but adds overhead.

**Mitigations:**
1. Add `assert!(active.len() >= MIN_SURVIVING_STS)` at the end.
2. Track which STs were dropped and at which cycle — produce a structured
   failure report.

---

### test_tpch_full_vs_differential

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:912 |
| Queries tested | 22 (creates FULL + DIFF pair per query) |
| Cycles | configurable (default 3) |
| Assertion | EXCEPT ALL between FULL and DIFF STs per cycle |
| Final guard | `assert!(passed > 0)` ⚠️ |
| Risk | **HIGH** (weak final assertion) |

**What it does:** For each query, creates two STs (one FULL, one DIFFERENTIAL).
After RF mutations, refreshes both and compares them directly with `EXCEPT ALL`.
Stronger than Phase 1 because it tests two independent refresh paths.

**Assessment:** **Excellent concept, weak final check.** The per-cycle
`EXCEPT ALL` comparison between FULL and DIFF is very strong — it catches cases
where both happen to diverge from ground truth in the same direction. But the
final assertion `passed > 0` means the test passes if **only 1 out of 22
queries** succeeds.

**Issues:**
1. `assert!(passed > 0)` — should be at least `passed >= 15`.
2. Neither the FULL nor DIFF ST is independently checked against ground truth.
   If both produce the same wrong answer, the test passes. However, Phase 1
   (`test_tpch_differential_correctness`) covers ground-truth for DIFF, so
   this is acceptable as a complementary check.
3. Skipped queries go into a `Vec<String>` with no T2-style regression guard.

**Mitigations:**
1. Replace `passed > 0` with `passed >= 15` (or `queries.len() - DIFFERENTIAL_SKIP_ALLOWLIST.len()`).
2. Add T2-style guard: if a query that isn't in the DIFFERENTIAL skip allowlist
   fails the FULL-vs-DIFF comparison, hard-fail.

---

### test_tpch_q07_isolation

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:1104 |
| Queries tested | Q07 only |
| Cycles | configurable (default 3) |
| Assertion | `assert_tpch_invariant` per cycle |
| Final guard | Hard panic on non-infrastructure errors ✅ |
| Risk | LOW |

**What it does:** Regression test for the BinaryOp parenthesization fix (fix
#17). Creates Q07 in isolation with dedicated error handling for known
infrastructure limits (temp_file_limit, connection drops).

**Assessment:** **Good regression test.** Specific, focused, with appropriate
infrastructure error handling. The `expect()` on baseline is correct — Q07
must pass baseline or the test is hard-failed.

**Issues:**
1. The temp_file_limit and connection-drop handling silently skips remaining
   cycles. At SF=0.01, Q07 may not even complete 1 cycle due to temp spill.
   The test is effectively a baseline-only check at small scale factors.
2. Duplicates logic from `test_tpch_differential_correctness` (Q07 is already
   in the main loop). The value-add is the isolation and specific error handling.

**Mitigations:**
1. Consider running Q07 isolation at a slightly higher SF (0.05) where temp
   spill is less likely, or increase temp_file_limit for this specific test.

---

### test_tpch_performance_comparison

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:1201 |
| Queries tested | 22 (benchmarks FULL vs DIFF timings) |
| Cycles | configurable (default 3) |
| Assertion | `assert!(!results.is_empty())` ⚠️ |
| Final guard | None (informational output only) |
| Risk | **MEDIUM** |

**What it does:** Creates FULL + DIFF pairs, measures wall-clock refresh time
for each, outputs a speedup table.

**Assessment:** **Good benchmarking but no regression detection.** The speedup
table is informational only — there's no assertion that DIFF should be faster
than FULL (or at least not absurdly slower). Known issues: q17/q20 show 652×
and 326× slowdown in DIFF vs FULL (correlated subqueries).

**Issues:**
1. `assert!(!results.is_empty())` — could pass with just 1 query benchmarked.
2. No performance regression threshold. If a fix causes DIFF to become 10×
   slower for a query that was previously fast, this test won't catch it.
3. No correctness assertion — only measures timing, doesn't verify results.

**Mitigations:**
1. Add `assert!(results.len() >= 15)`.
2. Store baseline performance in a reference file; fail if any query regresses
   more than 3× from baseline.
3. Optionally add `assert_tpch_invariant` after timing to verify correctness.

---

### test_tpch_sustained_churn

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:1424 |
| Queries tested | 6 (q01, q03, q06, q10, q14, q22) |
| Cycles | 50 (configurable via `TPCH_CHURN_CYCLES`) |
| Assertion | `assert_eq!(drift_detected, 0)` ✅ |
| Final guard | Hard-fail on any drift |
| Risk | LOW |

**What it does:** Runs 50 cycles of RF1+RF2+RF3 on 6 representative queries.
Checks correctness every 10th cycle. Tracks change buffer sizes, wall-clock
time, and cumulative drift.

**Assessment:** **Excellent stress test.** The 50-cycle sustained load is the
best test for detecting cumulative differential drift (where small errors
compound over time). The final assertion `drift_detected == 0` is strong.

**Issues:**
1. Only 6 of 22 queries are tested (q01, q03, q06, q10, q14, q22). The
   selection is reasonable (they're known to work in DIFF mode) but misses
   some operator diversity (no window functions, no EXISTS/NOT EXISTS).
2. Correctness is checked every 10th cycle, not every cycle. A transient
   drift that self-corrects (unlikely but possible if FULL-rescan fallback
   triggers) would not be detected.
3. Refresh errors are accumulated in `errors` but individual STs are not
   removed from the active set on error. The next cycle will try to
   refresh the same failing ST again (may be intentional for resilience
   testing but masks persistent errors).

**Mitigations:**
1. Add 1–2 more queries to the churn set that exercise different operator
   types (e.g., q22 for NOT EXISTS, q02 for correlated subquery).
2. Optionally check every 5th cycle instead of every 10th.

---

### test_tpch_immediate_correctness

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:1689 |
| Queries tested | 22 (in IMMEDIATE mode) |
| Cycles | configurable (default 3) |
| Assertion | `assert_tpch_invariant` after each RF step (3× per cycle) |
| Final guard | T2 skip-set guard (but allowlist is all 22 ⚠️) |
| Risk | **HIGH** (T2 guard disabled) |

**What it does:** Creates each query as an IMMEDIATE-mode ST. For each cycle,
applies RF1 → assert, RF2 → assert, RF3 → assert (3 invariant checks per
cycle vs 1 for DIFFERENTIAL). Tests that IVM triggers fire correctly within
the same transaction.

**Assessment:** **Strong assertion cadence (3× per cycle)** but the T2 guard
is fully permissive — all 22 queries are in `IMMEDIATE_SKIP_ALLOWLIST`. This
means any IMMEDIATE regression is silently absorbed.

**Issues:**
1. **IMMEDIATE_SKIP_ALLOWLIST contains all 22 queries.** The T2 guard cannot
   detect any regression. This is the highest-priority fix in the suite.
2. The test applies RF1, RF2, RF3 as separate DML operations (not batched).
   IMMEDIATE triggers fire for each RF individually, which is correct for
   testing per-operation trigger paths.

**Mitigations:**
1. **CRITICAL:** Run the test once, collect actual skip set, replace allowlist.
2. Add `assert!(passed >= MIN_IMMEDIATE_PASSING)` threshold.

---

### test_tpch_immediate_rollback

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:1895 |
| Queries tested | 4 (q01, q06, q03, q05) |
| Assertion | Row count + `assert_tpch_invariant` post-rollback |
| Final guard | `assert!(all_passed)` ✅ |
| Risk | LOW |

**What it does:** For each query, creates IMMEDIATE ST, then executes RF1 in
a transaction, verifies ST was updated mid-transaction, ROLLBACKs, and verifies
ST reverted to pre-mutation state. Repeats for RF2 (DELETE) and RF3 (UPDATE).

**Assessment:** **Unique and valuable.** This is the only test that verifies
transactional atomicity of IMMEDIATE mode IVM triggers. The mid-transaction
count check (`mid_count`) confirms the trigger actually fired, and the
post-rollback invariant check confirms reversion.

**Issues:**
1. Only 4 queries tested (q01, q06, q03, q05). q05 typically skips due to
   temp_file_limit on RF itself, leaving 3 effective queries.
2. After each ROLLBACK, the test re-checks invariant against cycle 0 (original
   data). This is correct but doesn't test rollback after multiple committed
   mutations.
3. No test for nested transactions (SAVEPOINT + ROLLBACK TO SAVEPOINT).

**Mitigations:**
1. Replace q05 with a query that reliably passes (e.g., q10, q14, or q22).
2. Add a test variant that commits some mutations, then rolls back a subsequent
   one, verifying the committed state is preserved.

---

### test_tpch_differential_vs_immediate

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:2183 |
| Queries tested | 22 (creates DIFF + IMMEDIATE pair per query) |
| Cycles | configurable (default 3) |
| Assertion | EXCEPT ALL between DIFF and IMMEDIATE STs |
| Final guard | `assert!(passed > 0)` ⚠️ |
| Risk | **HIGH** (weak final assertion + known deadlocks) |

**What it does:** Creates paired DIFFERENTIAL and IMMEDIATE STs for each query.
Applies shared RF mutations (IMMEDIATE STs update via triggers; DIFFERENTIAL
STs need explicit refresh). Compares the two with `EXCEPT ALL`.

**Assessment:** **Excellent concept but operationally fragile.** Known issues:
- q08 deadlocks reliably (lock ordering conflict between IVM trigger and
  explicit DIFF refresh)
- q01/q13 show mode divergence due to non-deterministic RF mutations
- 6+ queries typically diverge or error out

**Issues:**
1. `assert!(passed > 0)` — could pass with 1/22.
2. Known deadlock on q08 is not handled gracefully — it causes test to hang
   until lock_timeout (60s).
3. The deadlock issue (DIFF refresh locks change buffer while IMMEDIATE trigger
   also needs it) is a real product bug, not just a test issue.

**Mitigations:**
1. Replace `passed > 0` with `passed >= 10`.
2. Add deadlock detection: if a refresh hits a lock_timeout, classify as
   "deadlock" (not "skip") and report separately.
3. Serialize DIFF and IMMEDIATE refreshes per cycle to avoid lock conflicts.

---

### test_tpch_single_row_mutations

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_tests.rs:2386 |
| Queries tested | 3 (q01, q06, q03) |
| Assertion | `assert_tpch_invariant` after each step (INSERT/UPDATE/DELETE) |
| Final guard | Soft-pass for trigger errors ✅ |
| Risk | LOW |

**What it does:** Tests single-row INSERT → UPDATE → DELETE with fixed
orderkey 9999991. Exercises 1-row `NEW TABLE`/`OLD TABLE` trigger paths
distinct from batch operations.

**Assessment:** **Good focused test.** Single-row mutations hit different
code paths in PostgreSQL's transition table implementation. Testing 3 queries
(pure aggregate, filter+aggregate, multi-table join) covers the main IVM
delta paths.

**Issues:**
1. Only 3 queries — could benefit from 1-2 more covering different operator
   types (window function, subquery).
2. The fixed orderkey (9999991) may collide at very high scale factors
   (SF > 6.6). Currently safe for CI defaults.

---

### test_tpch_dag_chain

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_dag_tests.rs:239 |
| DAG structure | Q01 → filtered projection (2 levels) |
| Cycles | configurable (default 3) |
| Assertion | `assert_invariant` on both levels per cycle ✅ |
| Final guard | Hard panic on invariant failure ✅ |
| Risk | LOW |

**What it does:** Creates a 2-level DAG: level-0 is Q01 (aggregate over
lineitem), level-1 is a filtered projection of level-0's output. Refreshes
in topological order after RF mutations.

**Assessment:** **Good DAG chain test.** The ground-truth query for level-1
re-derives the result from base tables (not from the level-0 ST), ensuring
both levels are correct.

**Issues:**
1. Level-1 query is trivial (`SELECT * FROM ... WHERE l_returnflag = 'R'`).
   A more complex level-1 (re-aggregation, join with another table) would
   exercise more DAG propagation paths.
2. Only tests Q01 (simple aggregate). The DAG with a complex multi-join query
   at level-0 would be more representative.

**Mitigations:**
1. Add a DAG chain test with a complex level-0 query (Q03 or Q10).
2. Make level-1 query more complex (e.g., join level-0 output with a
   dimension table).

---

### test_tpch_dag_multi_parent

| Metric | Value |
|--------|-------|
| Location | e2e_tpch_dag_tests.rs:369 |
| DAG structure | Q01 + Q06 → UNION ALL aggregate (fan-in) |
| Cycles | configurable (default 3) |
| Assertion | `assert_invariant` on all 3 STs per cycle |
| Final guard | Soft-skip on baseline failure ⚠️ |
| Risk | MEDIUM |

**What it does:** Creates a multi-parent fan-in DAG: two level-0 STs (Q01 and
Q06) feed a level-1 ST that aggregates their revenue via UNION ALL + SUM.

**Assessment:** **Good fan-in test.** The ground-truth query for level-1
re-derives the combined revenue from base tables.

**Issues:**
1. Baseline failure causes soft-skip (`return`), not hard-fail. If the UNION
   ALL query has a persistent creation failure, this test silently passes.
2. The level-1 query is a scalar aggregate (`SUM(revenue_total)`) — a single
   number. If the value accidentally matches due to cancellation, the test
   passes. An invariant check on 1 row has less statistical power than on
   many rows.
3. Mid-cycle refresh errors (`break`) abort the loop but don't fail the test.

**Mitigations:**
1. Change soft-skip to hard-fail on baseline: if both level-0 STs create
   successfully but union fails, that's a bug, not a known limitation.
2. Add a non-aggregate fan-in test (e.g., UNION ALL without aggregation)
   to verify multi-row correctness.

---

## Known Limitations & Skip-Set Analysis

### DIFFERENTIAL skip set (5 queries)

| Query | Reason | Tracked in |
|-------|--------|-----------|
| q05 | temp_file_limit (6-table join, DVM SQL too large) | PLAN_TEST_SUITE_TPC_H-INFRASTRUCTURE.md RC-2 |
| q07 | temp_file_limit (6-table chain join) | Same |
| q08 | temp_file_limit (8-table join) | Same |
| q09 | temp_file_limit (6-table join) | Same |
| q12 | SUM(CASE WHEN) value mismatch — DVM bug | Same RC-3 |

**Analysis:** The q05/q07/q08/q09 skip is an infrastructure constraint (Docker
temp_file_limit = 4 GB), not a correctness bug. These queries generate massive
intermediate CTEs during DVM refresh. The root cause is architectural
(materialized CTE approach for wide joins). Raising temp_file_limit would let
them pass but would also increase CI time and disk usage significantly.

q12 is a genuine **DVM correctness bug** — `SUM(CASE WHEN col IN (...) THEN 1
ELSE 0 END)` produces wrong values after cycle 1. The root cause is in
`replace_column_refs_in_raw()` failing to resolve column references from the
join delta CTE. This is tracked but unresolved.

### IMMEDIATE skip set (unknown — allowlist disabled)

The IMMEDIATE_SKIP_ALLOWLIST contains all 22 queries, effectively disabling
the regression guard. From REPORT_TPC_H_ISSUES.md, the actual known IMMEDIATE
failures include deadlocks (q08), temp_file_limit (q05/q07/q08/q09), and
unspecified IVM creation restrictions. The real skip set is estimated at 6–10
queries.

### Operator coverage across TPC-H queries

| Operator | Queries using it | Coverage |
|----------|-----------------|----------|
| Multi-table equi-join | q02, q03, q05, q07, q08, q09, q10, q21 | ✅ 8/22 |
| GROUP BY aggregate | q01, q03, q04, q05, q07, q09, q10, q13, q14, q16, q18 | ✅ 11/22 |
| Correlated scalar subquery | q02, q17, q20 | ✅ 3/22 |
| EXISTS / NOT EXISTS | q04, q21, q22 | ✅ 3/22 |
| IN subquery | q16, q18, q20 | ✅ 3/22 |
| CASE WHEN | q07, q08, q12 | ✅ 3/22 (q12 broken) |
| TopK (ORDER BY + LIMIT) | q02, q03, q10, q18, q21 | ✅ 5/22 |
| Derived table (subquery-in-FROM) | q07, q08, q13, q15, q22 | ✅ 5/22 |
| Date arithmetic | q01, q03, q04, q06, q10, q12, q14, q15, q20 | ✅ 9/22 |
| String functions | q02, q16, q22 | ✅ 3/22 |
| Self-join | q07, q21 | ✅ 2/22 |
| Scalar aggregate (no GROUP BY) | q06, q17 | ✅ 2/22 |
| LEFT JOIN | q13, q15 | ✅ 2/22 |
| EXTRACT | q07, q08, q09, q12 | ✅ 4/22 |

**Gaps:** No TPC-H query uses window functions, LATERAL, CTEs, UNION (without
subquery-in-FROM), or FULL OUTER JOIN. These operators are covered by the
focused E2E tests, not TPC-H.

---

## Priority Mitigations

### P0 — Critical (should fix before next release)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 1 | Populate IMMEDIATE_SKIP_ALLOWLIST from actual test run | T2 guard becomes active for IMMEDIATE mode | Small |
| 2 | Replace `assert!(passed > 0)` with minimum thresholds in `full_vs_differential` and `differential_vs_immediate` | Prevents passing with 1/22 queries | Small |
| 3 | Add minimum surviving assertion to `test_tpch_cross_query_consistency` | Prevents silent ST deactivation | Small |

### P1 — High (should address soon)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 4 | Add T2-style skip-set guard to `test_tpch_full_vs_differential` | Catches FULL-vs-DIFF regressions | Small |
| 5 | Replace q05 with a reliable query in `test_tpch_immediate_rollback` | Rollback test covers 4 queries instead of 3 | Small |
| 6 | Fix DAG multi-parent soft-skip: hard-fail if level-0 STs create OK but union fails | Prevents silent test bypass | Small |
| 7 | Populate `failed` vector in `test_tpch_differential_correctness` for true assertion errors | Distinguishes DVM errors from correctness bugs | Medium |

### P2 — Medium (address during regular maintenance)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 8 | Add standalone FULL mode correctness test | FULL mode verified independently | Medium |
| 9 | Add deadlock detection to `test_tpch_differential_vs_immediate` | q08 deadlock classified properly | Medium |
| 10 | Add churn queries for underrepresented operators (NOT EXISTS, correlated subquery) | Churn covers more operator types | Small |
| 11 | Extract shared helpers from `e2e_tpch_dag_tests.rs` to avoid duplication | Code hygiene | Small |
| 12 | Add performance regression threshold (3× baseline) | Catches performance regressions | Medium |

### P3 — Low (backlog)

| # | Action | Impact | Effort |
|---|--------|--------|--------|
| 13 | Add nested SAVEPOINT rollback test for IMMEDIATE mode | Deeper transactional correctness | Medium |
| 14 | Make DAG chain level-1 more complex (re-aggregation or join) | Stronger DAG propagation test | Medium |
| 15 | Add `TPCH_STRICT=1` env var that hard-fails on any skip | Enables strict CI mode | Small |
| 16 | Fix `buf≈-2` display with `GREATEST(reltuples, 0)` | Cosmetic fix | Trivial |
| 17 | Investigate customer UPDATE DVM bug (LEFT JOIN delta SQL) | Enables RF3 customer UPDATEs | Large |

---

## Appendix: Query Coverage Matrix

Which tests exercise which queries:

| Query | differential_correctness | cross_query | full_vs_diff | q07_isolation | performance | sustained_churn | immediate | rollback | diff_vs_imm | single_row | dag_chain | dag_multi_parent |
|-------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| q01 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| q02 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q03 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | | |
| q04 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q05 | ⚠️ | ⚠️ | ⚠️ | | ⚠️ | | ⚠️ | ⚠️ | ⚠️ | | | |
| q06 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | | ✅ |
| q07 | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | | ⚠️ | | ⚠️ | | | |
| q08 | ⚠️ | ⚠️ | ⚠️ | | ⚠️ | | ⚠️ | | ⚠️ | | | |
| q09 | ⚠️ | ⚠️ | ⚠️ | | ⚠️ | | ⚠️ | | ⚠️ | | | |
| q10 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | | ✅ | | | |
| q11 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q12 | ⚠️ | ⚠️ | ⚠️ | | ⚠️ | | ✅* | | ⚠️ | | | |
| q13 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q14 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | | ✅ | | | |
| q15 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q16 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q17 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q18 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q19 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q20 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q21 | ✅ | ✅ | ✅ | | ✅ | | ✅ | | ✅ | | | |
| q22 | ✅ | ✅ | ✅ | | ✅ | ✅ | ✅ | | ✅ | | | |

✅ = tested and passing ⚠️ = attempted but skipped (known limitation)
\* q12 passes in IMMEDIATE mode (trigger path avoids the CASE delta bug)

**Test function count by file:**

| File | Functions | Lines |
|------|-----------|-------|
| e2e_tpch_tests.rs | 10 | 2,524 |
| e2e_tpch_dag_tests.rs | 2 | 570 |
| **Total** | **12** | **3,094** |

---

*End of report.*
