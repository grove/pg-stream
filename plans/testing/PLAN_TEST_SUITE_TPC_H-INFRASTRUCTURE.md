# TEST_SUITE_TPC_H-INFRASTRUCTURE.md — TPC-H Failure Resolution Plan

> **TPC-H Fair Use:** This workload is *derived from* the TPC-H Benchmark
> specification but does **not** constitute a TPC-H Benchmark result.
> "TPC-H" and "TPC Benchmark" are trademarks of the Transaction Processing
> Performance Council ([tpc.org](https://www.tpc.org/)).

**Status:** Proposed  
**Date:** 2026-03-09  
**Branch:** `fix-tpch-infrastructure`  
**Scope:** Resolution of the failures and skips observed in
`/tmp/tpch_run.log`. Three root causes were identified across two
infrastructure issues and one DVM correctness regression.

---

## Observed Failures

| Test | Queries | Symptom | Cycle |
|------|---------|---------|-------|
| `test_tpch_differential_correctness` | q05, q07, q08, q09 | `could not write to file ... temp_file_limit exceeded` | Cycle 1 |
| `test_tpch_sustained_churn` | churn_q05 | `refresh skipped: ... another refresh is already in progress` | All cycles |
| `test_tpch_differential_correctness` | q12 | Invariant violation: `(MAIL, high=0, low=5)` in ST, `(MAIL, high=1, low=5)` expected | Cycle 1 |

All three failures on the same run: q05 hits `temp_file_limit` first, leaves
its advisory lock unreleased, and `churn_q05` (which shares the same
stream-table OID / lock key) then stalls for the remainder of the run.
q12 is an independent DVM correctness regression.

---

## Root Cause Analysis

### RC-1 — Advisory lock not released when transaction aborts

**File:** `src/api.rs`

The `pgtrickle.refresh_stream_table()` function acquires a session-level
advisory lock with `pg_try_advisory_lock($1)` before calling
`execute_manual_refresh()`, then unconditionally calls `pg_advisory_unlock($1)`
afterwards:

```rust
// src/api.rs ~line 1910
let got_lock = Spi::get_one_with_args::<bool>(
    "SELECT pg_try_advisory_lock($1)", &[st.pgt_id.into()]
)?;
if !got_lock { return Err(PgTrickleError::RefreshSkipped(...)); }

let result = execute_manual_refresh(...);

// BUG: This SPI call silently fails if the transaction is in the
//      aborted state caused by an earlier error (e.g. temp_file_limit).
let _ = Spi::get_one_with_args::<bool>(
    "SELECT pg_advisory_unlock($1)", &[st.pgt_id.into()]
);
```

`pg_try_advisory_lock` is **session-level**: it survives transaction aborts.
When `execute_manual_refresh` triggers a PostgreSQL error (e.g.
`temp_file_limit exceeded`), pgrx unwinds into an aborted transaction state.
The subsequent `pg_advisory_unlock` SPI call then fails silently because no
SPI call can execute in an aborted transaction — pgrx either returns `Err`
or generates an internal error, and the `let _ =` discards it. The lock
remains held on the connection; since sqlx reuses pooled connections, all
subsequent refreshes on that stream table discover the lock still taken and
return `RefreshSkipped`.

### RC-2 — `temp_file_limit` too small for deep-join DVM queries

**File:** `tests/e2e/mod.rs` (`new_with_db_bench()`)

The bench variant of `E2eDb` sets `temp_file_limit = '4GB'` and
`work_mem = '64MB'`. At scale factor SF=0.01 the DIFFERENTIAL DVM generates
multi-CTE delta SQL for complex joins. The `use_pre_change_snapshot` gate in
`src/dvm/operators/join_common.rs` activates the L₁ + correction path for
joins with more than 2 scan nodes (i.e., 3 or more source tables). This path
materialises large intermediate CTEs that spill to disk at 64 MB of
`work_mem`. At SF=0.01:

| Query | Join width | Observed behaviour |
|-------|-----------|-------------------|
| q05 | 5 tables | `temp_file_limit exceeded` cycle 1 RF1 |
| q07 | 6 tables | `temp_file_limit exceeded` cycle 1 RF1 |
| q08 | 8 tables | `temp_file_limit exceeded` cycle 1 RF1 |
| q09 | 6 tables | `temp_file_limit exceeded` cycle 1 RF1 |

Raising both settings in the bench container allows these queries to complete
without spilling to disk (SF=0.01 is still very small; the issue is purely
the intermediate CTE expansion).

### RC-3 — q12 `SUM(CASE WHEN …)` algebraic delta miscompute

**Files:** `src/dvm/operators/aggregate.rs`, `src/dvm/operators/filter.rs`

Q12 uses a `SUM(CASE WHEN o_orderpriority = '1-URGENT' OR … THEN 1 ELSE 0 END)`
expression. Because `is_algebraically_invertible` returns `true` for
`AggFunc::Sum` unconditionally, the algebraic delta path is taken:

```
new_agg = old_agg + ins_sum - del_sum
```

where `ins_sum` = `SUM(CASE WHEN action='I' THEN <resolved_case_expr> ELSE 0 END)`
over the join delta rows. The `<resolved_case_expr>` is the CASE expression
with column references re-mapped to the join delta CTE's output columns by
`replace_column_refs_in_raw`.

The observed mismatch (`high_line_count` off by 1 for MAIL, `low_line_count`
off by 1 for SHIP simultaneously) indicates that exactly one insert delta row
was mishandled — `ins_sum` evaluated to 0 for a row that should contribute 1,
causing `new_agg = old_agg + 0 - 0 = old_agg` (no update) while the true
answer is `old_agg + 1`.

**Likely causes** (to confirm during implementation):

1. **Ambiguous column disambiguation** — if `replace_column_refs_in_raw`
   encounters `o_orderpriority` and the join delta CTE happens to expose both
   `orders__o_orderpriority` and some other `__o_orderpriority`-suffixed
   column, the `seen_bases` dedup logic skips the replacement (marks it
   "ambiguous"). The raw `o_orderpriority` reference then resolves to `NULL`
   or an error at query execution time rather than the expected integer.

2. **Double-quoted vs unquoted column names in raw SQL** — `replace_column_refs_in_raw`
   uses word-boundary matching for plain identifiers. If the raw SQL of the
   CASE expression contains `"o_orderpriority"` (with double quotes, as
   generated by some parser paths), the unquoted replacement path does not
   fire. The delta CTE selects the column as `"orders__o_orderpriority"` but
   the CASE comparison still references the qualified form.

3. **CASE result type coercion** — `SUM(CASE WHEN … THEN 1 ELSE 0 END)` where
   the THEN/ELSE literals are `integer`. After delta re-wrapping the CASE
   expression may lose its type, silently coercing to `NULL`.

The correct fix must ensure the CASE expression inside `SUM(…)` properly
references the join delta CTE column names so that `ins_sum` ≠ 0 for rows
where the CASE condition holds.

---

## Implementation Plan

### Fix 1 — Switch to transaction-level advisory lock (HIGH PRIORITY)

**Goal:** Ensure the advisory lock is always released even when the
underlying PostgreSQL transaction aborts.

**File:** `src/api.rs`

**Change:** Replace session-level `pg_try_advisory_lock` /
`pg_advisory_unlock` with the transaction-level variants
`pg_try_advisory_xact_lock` / *(no explicit unlock needed)*.

Transaction-level advisory locks are automatically released at the end of the
transaction (commit or rollback), including on error-triggered rollbacks.
There is no corresponding `pg_advisory_xact_unlock`; PostgreSQL releases them
automatically.

**Diff sketch:**

```diff
- "SELECT pg_try_advisory_lock($1)"
+ "SELECT pg_try_advisory_xact_lock($1)"

- // After execute_manual_refresh:
- let _ = Spi::get_one_with_args::<bool>(
-     "SELECT pg_advisory_unlock($1)", &[st.pgt_id.into()]
- );
```

**Acceptance criteria:**
- `test_tpch_sustained_churn` completes all cycles without "refresh skipped"
  errors after a prior cycle hit `temp_file_limit`.
- Existing advisory lock behavior is preserved: concurrent `refresh_stream_table`
  calls on the same stream table still return `RefreshSkipped` during an
  active refresh.
- Unit test: add a test in `src/api.rs` (or integration test) that verifies
  a failed refresh does not leave the lock held by running a second
  `refresh_stream_table` call after a simulated error and checking that it
  does NOT return `RefreshSkipped`.

**Risk:** LOW — the semantic change is purely in lock lifetime (end-of-xact
vs. explicit unlock). The only behavioral difference is that a single
transaction cannot call `refresh_stream_table` on the same table twice (the
lock would be held for the entire transaction). This pattern is not used
anywhere in the codebase and is not a supported use-case.

---

### Fix 2 — Raise bench container memory and temp-file limits (MEDIUM PRIORITY)

**Goal:** Allow q05, q07, q08, q09 to complete DIFFERENTIAL refresh cycles
without hitting disk-spill limits at SF=0.01.

**File:** `tests/e2e/mod.rs` (`new_with_db_bench()`)

**Changes:**

```diff
- db.execute("ALTER SYSTEM SET work_mem = '64MB'").await?;
+ db.execute("ALTER SYSTEM SET work_mem = '256MB'").await?;

- db.execute("ALTER SYSTEM SET temp_file_limit = '4GB'").await?;
+ db.execute("ALTER SYSTEM SET temp_file_limit = '16GB'").await?;
```

Also verify that the Docker container used in CI (either the testcontainers
stock `postgres:18.1` image or the custom E2E image) has sufficient `/tmp`
disk space. The E2E Dockerfile (`tests/Dockerfile.e2e`) does not restrict
`/tmp`, so the host's disk space is the practical limit; 16 GB should be
well within range for any CI runner with a standard ephemeral disk.

**Rationale for 256 MB work_mem:** At SF=0.01 the largest in-memory sort for
q08 (8-table join delta) is approximately 180–220 MB. Setting 256 MB
eliminates sorting spill for all 22 queries at this scale factor, while
remaining well within the 4 GB SHM limit already configured
(`with_shm_size(268_435_456)` is 256 MB; this may also need to be raised —
see below).

**Docker SHM note:** The testcontainers `with_shm_size` call in
`new_with_db_bench()` currently sets 256 MB. PostgreSQL's shared memory usage
grows with `work_mem` × `max_connections`. Raise to at least 512 MB:

```diff
- .with_shm_size(268_435_456)   // 256 MB
+ .with_shm_size(536_870_912)   // 512 MB
```

**Acceptance criteria:**
- `test_tpch_differential_correctness` passes for q05, q07, q08, q09 with
  zero `temp_file_limit exceeded` errors across all configured cycles.
- No other test that uses `new_with_db_bench()` regresses.
- CI peak disk usage (measured by `du -sh /tmp` in the E2E Docker container)
  remains under 12 GB.

**Risk:** LOW-MEDIUM — higher `work_mem` could change query plans (e.g., hash
joins become more likely). If any test has plan-sensitive assertions, they may
need updating. In practice all TPC-H invariant checks are multiset-equality
assertions against the live query, so plan changes are fine.

---

### Fix 3 — Repair `SUM(CASE WHEN …)` column resolution in join delta (MEDIUM PRIORITY)

**Goal:** Ensure `SUM(CASE WHEN col = … THEN 1 ELSE 0 END)` inside an
aggregate delta correctly resolves column references from the join delta CTE
so that `ins_sum` ≠ 0 for rows where the CASE condition holds.

**Investigation steps (do first):**

1. Add a `pgrx::warning!()` or `pgrx::log!()` in `agg_delta_exprs` that
   prints the resolved `col` string for each `AggFunc::Sum` with a
   `Expr::Raw` argument. Run `test_tpch_differential_correctness` in isolation
   on q12 to capture the generated delta SQL and compare against the join
   delta CTE column list.

2. Examine `replace_column_refs_in_raw` for q12:
   - `child_cols` will contain (approximately):
     `["orders__o_orderkey", "orders__o_orderstatus", "orders__o_orderpriority", …, "lineitem__l_shipmode", …]`
   - The raw CASE expression from the parser will be something like:
     `CASE WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH' THEN 1 ELSE 0 END`
     or possibly the qualified form:
     `CASE WHEN "orders"."o_orderpriority" = '1-URGENT' OR "orders"."o_orderpriority" = '2-HIGH' THEN 1 ELSE 0 END`
   - In the first form, `replace_column_refs_in_raw` maps `o_orderpriority` →
     `"orders__o_orderpriority"` (unambiguous suffix match). If this mapping
     fires correctly, the CASE should work.
   - In the second form, `replace_qualified_column_refs` must handle the
     `"table"."column"` pattern and substitute the disambiguated name. Verify
     that this path handles double-quoted identifiers inside CASE expressions
     when they are wrapped in the aggregate delta instrumentation.

3. Capture the full generated delta SQL for q12 by adding a temporary
   `pgrx::warning!("delta SQL: {}", delta_sql)` and running the test against
   a Docker container. Confirm whether `orders__o_orderpriority` appears in
   the emitted SQL.

**Fix options:**

**Option A (conservative) — disable algebraic path for `Raw` arguments:**

```rust
// src/dvm/operators/aggregate.rs
fn is_algebraically_invertible(agg: &AggExpr) -> bool {
    if agg.is_distinct { return false; }
    // Do not use the algebraic path when the argument is an opaque Raw
    // expression. Column references inside Raw SQL are resolved by text
    // substitution which may silently produce wrong results if the
    // substitution pattern mismatches the join delta CTE's output columns.
    if matches!(&agg.argument, Some(Expr::Raw(_))) { return false; }
    matches!(agg.function, AggFunc::CountStar | AggFunc::Count | AggFunc::Sum)
}
```

This forces the group-rescan path for `SUM(CASE WHEN …)`, which is correct
at the cost of a full group re-evaluation on every delta batch. For queries
like q12 (simple aggregation over a 2-table join), the rescan cost is small.

**Option B (preferred) — fix `replace_column_refs_in_raw` for `Raw` CASE in
join context:**

Confirm whether `replace_qualified_column_refs` correctly handles:
```
CASE WHEN "orders"."o_orderpriority" = '1-URGENT' THEN 1 ELSE 0 END
```
mapping `"orders"."o_orderpriority"` → `"orders__o_orderpriority"`. If this
does not fire, add handling in `replace_qualified_column_refs` for the
double-quote qualified form to produce the disambiguated CTE column name. Add
a unit test directly in `filter.rs`:

```rust
#[test]
fn test_replace_qualified_refs_case_expression() {
    let sql = r#"CASE WHEN "orders"."o_orderpriority" = '1-URGENT' THEN 1 ELSE 0 END"#;
    let child_cols = vec!["orders__o_orderpriority".to_string()];
    let result = replace_column_refs_in_raw(sql, &child_cols);
    assert!(
        result.contains("orders__o_orderpriority"),
        "CASE expression column not resolved: {result}"
    );
}
```

If the unit test reveals the substitution correctly fires, the bug lies
elsewhere (possibly in the double-wrapping of the CASE inside the
delta-action guard: `... THEN (CASE WHEN … END) ELSE 0 END`). In that case,
debug the generated SQL string directly.

**Recommendation:** Implement Option A first as a safe regression guard, then
pursue Option B to restore the algebraic optimisation. Both can land in the
same PR.

**Acceptance criteria:**
- `test_tpch_differential_correctness` passes q12 for all configured cycles
  with 0 invariant violations.
- No existing TPC-H query that previously used the algebraic path regresses
  (verify by checking that queries q01, q13 — which use `SUM(col)` and
  `COUNT(*)` with plain column arguments — still pass and do not switch to
  the group-rescan path).
- New unit test `test_replace_qualified_refs_case_expression` passes.

**Risk:** LOW (Option A) — the group-rescan path is the well-tested fallback.
MEDIUM (Option B) — textual SQL manipulation is fragile; changes must be
covered by unit tests.

---

## Sequencing

```
Fix 1 (api.rs advisory lock)    — independent, do first
Fix 2 (bench memory limits)     — independent, do in parallel with Fix 1
Fix 3 (q12 CASE column resolve) — investigate first, then fix
```

Suggested branch order:
1. `fix-advisory-lock-xact` — Fix 1 only; one-line change + test
2. `fix-bench-memory-limits` — Fix 2 only; two-line change in `mod.rs`
3. `fix-sum-case-algebraic`  — Fix 3; investigation + unit test + code change

Each branch can be PR'd independently.

---

## Validation

After all three fixes land, run the full TPC-H suite:

```bash
just build-e2e-image
cargo test --test e2e_tpch_tests -- --ignored --test-threads=1 --nocapture 2>&1 | tee /tmp/tpch_validation.log
```

Expected outcome:
- 0 queries skipped due to advisory lock cascade
- 0 `temp_file_limit exceeded` errors
- q12 invariant check passes every cycle
- All 6 test functions pass (≥ 22/22 queries per function where applicable)

---

## References

- `src/api.rs` — advisory lock acquisition/release pattern (~line 1905–1930)
- `src/dvm/operators/aggregate.rs` — `is_algebraically_invertible`, `agg_delta_exprs`
- `src/dvm/operators/filter.rs` — `replace_column_refs_in_raw`, `replace_qualified_column_refs`
- `src/dvm/operators/join_common.rs` — `use_pre_change_snapshot`, `join_scan_count`
- `tests/e2e/mod.rs` — `new_with_db_bench()` container configuration
- `plans/testing/PLAN_TEST_SUITE_TPC_H.md` — original fix log with root causes
- `plans/testing/TEST_SUITE_TPC_H-PART_2.md` — second-wave improvements (T1–T6)
- PostgreSQL docs — [`pg_advisory_xact_lock`](https://www.postgresql.org/docs/current/functions-admin.html#FUNCTIONS-ADVISORY-LOCKS)
