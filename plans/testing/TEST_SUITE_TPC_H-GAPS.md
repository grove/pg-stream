# TEST_SUITE_TPC_H-PART_2.md — TPC-H Test Suite Enhancements

> **TPC-H Fair Use:** This workload is *derived from* the TPC-H Benchmark
> specification but does **not** constitute a TPC-H Benchmark result.
> "TPC-H" and "TPC Benchmark" are trademarks of the Transaction Processing
> Performance Council ([tpc.org](https://www.tpc.org/)).

**Status:** Proposed  
**Date:** 2026-03-09  
**Branch:** `test-suite-tpc-h-part-2`  
**Scope:** Second wave of TPC-H test suite improvements, building on the
complete 22/22 passing baseline from `PLAN_TEST_SUITE_TPC_H.md`.

---

## Motivation

The original TPC-H suite (Phase 1–3 + sustained churn) proves the core DBSP
invariant for DIFFERENTIAL and FULL refresh modes. Now that 22/22 queries
pass cleanly, there are meaningful correctness gaps that require new tests
rather than new DVM fixes:

1. **IMMEDIATE mode is not exercised** — The IVM trigger path (`TransitionTable`
   delta source, statement-level AFTER triggers) is completely different from
   the DIFFERENTIAL path (`ChangeBuffer`, background refresh). The only IVM
   coverage today is `e2e_topk_tests.rs`, which uses simple ORDER BY + LIMIT
   queries. Complex join/aggregate queries are not tested.

2. **ROLLBACK correctness is not tested anywhere** — In IMMEDIATE mode, the
   IVM trigger fires inside the user's transaction. A rollback must revert
   the stream table update atomically. No test verifies this property.

3. **Negative `__pgt_count` is not detected** — The current invariant check
   uses multiset equality (EXCEPT ALL). A `__pgt_count < 0` bug can be masked
   if extra and missing rows accidentally cancel out. A direct check costs
   nothing and would catch a whole class of over-retraction bugs.

4. **The skipped-query set is not regression-guarded** — Queries that fail
   `create_stream_table()` are soft-skipped with a logged reason. There is no
   mechanism to detect if a previously-passing query is newly skipped (a DVM
   regression). The test passes silently even if the skip set grows.

5. **DIFFERENTIAL vs IMMEDIATE mode agreement is not tested** — The existing
   suite compares FULL vs DIFFERENTIAL. Comparing DIFFERENTIAL vs IMMEDIATE
   would catch cases where both incremental paths diverge from each other but
   happen to agree with the ground-truth query on the same cycle.

6. **Single-row mutations hit different code paths** — All existing RF
   mutations are batch operations (RF_COUNT ≥ 10 rows). Single-row INSERT,
   DELETE, and UPDATE trigger different transition table behaviour (1-row
   `NEW TABLE`/`OLD TABLE`). The trigger path for small batches is not
   explicitly validated.

7. **DAG chains are not exercised** — No TPC-H test creates a stream table
   whose defining query references another stream table, which is the primary
   use-case for the DAG-aware refresh scheduler.

---

## Goals

1. Add `test_tpch_immediate_correctness` — full 22-query IMMEDIATE mode
   correctness test with per-operation trigger assertions. *(DONE — merged
   in PR #135)*
2. Add `test_tpch_immediate_rollback` — verifies that a rolled-back DML
   transaction leaves the IVM stream table unchanged.
3. Strengthen `assert_tpch_invariant` with a `__pgt_count` sanity check.
4. Add a skip-set regression guard to `test_tpch_differential_correctness`
   and `test_tpch_immediate_correctness`.
5. Add `test_tpch_differential_vs_immediate` — side-by-side mode comparison.
6. Add `test_tpch_single_row_mutations` — single-row INSERT/DELETE/UPDATE in
   IMMEDIATE mode on a representative subset of queries.
7. Add `test_tpch_dag_chain` — a two-level DAG chain using two TPC-H queries,
   verifying end-to-end DAG correctness under mutations.

---

## Non-Goals

- SQL workaround removal (LIKE, NULLIF, COUNT DISTINCT) — tracked under R3 in
  the original plan.
- Upgrade-path TPC-H test (create on version N, upgrade, re-assert) — tracked
  separately in the upgrade test plan.
- TPC-H benchmarks (wall-clock throughput, SF > 0.1) — tracked in
  `PLAN_BENCHMARK.md`.

---

## Implementation Plan

### T1 — `__pgt_count` Sanity Check in `assert_tpch_invariant`

**File:** `tests/e2e_tpch_tests.rs`  
**Priority:** Highest — cheap to add, catches a whole class of bugs.

After the multiset-equality check, add:

```rust
// Negative __pgt_count is always a DVM bug (over-retraction).
// Check early so it surfaces even if EXCEPT ALL happens to cancel out.
let neg_count: i64 = db
    .query_scalar(&format!(
        "SELECT count(*) FROM {st_table} WHERE __pgt_count < 0"
    ))
    .await;
if neg_count > 0 {
    return Err(format!(
        "NEGATIVE __pgt_count: {qname} cycle {cycle} — \
         {neg_count} rows with __pgt_count < 0 (over-retraction bug)"
    ));
}
```

Guard the query with a column-existence check so it degrades gracefully for
stream tables that don't use `__pgt_count` (e.g. FULL mode, no-aggregate
queries):

```rust
let has_pgt_count: bool = db
    .query_scalar(&format!(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.columns
            WHERE table_name = '{st_name}'
              AND column_name = '__pgt_count'
        )"
    ))
    .await;
if has_pgt_count { /* run the negative count check */ }
```

This improvement applies automatically to every existing test that calls
`assert_tpch_invariant` — no callers need changing.

---

### T2 — Skip-Set Regression Guard

**File:** `tests/e2e_tpch_tests.rs`  
**Priority:** High — prevents silent regressions as DVM evolves.

Introduce a compile-time allowlist of queries that are permitted to be skipped
in each mode. If a newly-skipped query is not in the allowlist, the test fails.

```rust
/// Queries allowed to be skipped in DIFFERENTIAL mode.
/// Update this list when a DVM limitation is intentionally accepted.
/// Remove entries when the limitation is fixed.
const DIFFERENTIAL_SKIP_ALLOWLIST: &[&str] = &[
    // Currently empty — all 22 queries pass DIFFERENTIAL mode.
];

/// Queries allowed to be skipped in IMMEDIATE mode.
/// Populated from the first run of test_tpch_immediate_correctness.
/// Update as IMMEDIATE mode support is extended.
const IMMEDIATE_SKIP_ALLOWLIST: &[&str] = &[
    // Populated after first run — see test output for initial values.
];
```

At the end of each test, after the existing `skipped` summary:

```rust
let unexpected_skips: Vec<&str> = skipped
    .iter()
    .map(|(name, _)| *name)
    .filter(|name| !ALLOWLIST.contains(name))
    .collect();
assert!(
    unexpected_skips.is_empty(),
    "NEW REGRESSION: queries newly skipped that were not in the allowlist: {:?}\n\
     If intentional, add to the allowlist with a comment explaining why.",
    unexpected_skips
);
```

**Important:** The allowlist must be populated from an actual test run before
the guard is enabled. The initial population step is:

1. Run `just test-tpch` with the guard disabled (comment the assert out).
2. Collect the actual skipped set from the output.
3. Populate the allowlist.
4. Re-enable the assert and commit.

---

### T3 — `test_tpch_immediate_rollback`

**File:** `tests/e2e_tpch_tests.rs`  
**Priority:** High — ROLLBACK correctness is a fundamental IMMEDIATE mode
property not covered anywhere else.

**Design:**

For a representative subset of 3–4 queries covering different operator classes
(aggregate, join, aggregate+join), run the following pattern per query:

```
1. Create ST in IMMEDIATE mode.
2. Assert baseline invariant (snapshot the ST contents).
3. BEGIN transaction.
4. Apply RF1 mutations (INSERTs) — triggers fire, ST is updated.
5. Assert ST has changed (sanity check — mutation was visible).
6. ROLLBACK.
7. Assert ST matches pre-mutation state exactly.
8. Repeat with RF2 (DELETE) and RF3 (UPDATE).
9. Drop ST.
```

Step 7 is the key assertion: the ST must be identical (multiset-equal) to its
state before the rolled-back transaction. This verifies that PostgreSQL's
transactional behaviour naturally provides IMMEDIATE mode rollback safety
(triggers participate in the surrounding transaction, so their effects
roll back automatically).

**Query subset for this test:**

| Query | Operator class |
|-------|---------------|
| q01   | Scalar aggregate (no GROUP BY) |
| q06   | Filter + aggregate |
| q03   | Multi-table join + aggregate |
| q05   | 6-table join + GROUP BY aggregate |

These cover the main delta paths (INSERT-only aggregates, DELETE-only paths,
UPDATE paths) without requiring all 22 queries (which would make the test
slow).

**Implementation sketch:**

```rust
#[tokio::test]
#[ignore]
async fn test_tpch_immediate_rollback() {
    let db = E2eDb::new_bench().await.with_extension().await;
    load_schema(&db).await;
    load_data(&db).await;

    let rollback_queries: &[(&str, &str)] = &[
        ("q01", include_str!("tpch/queries/q01.sql")),
        ("q06", include_str!("tpch/queries/q06.sql")),
        ("q03", include_str!("tpch/queries/q03.sql")),
        ("q05", include_str!("tpch/queries/q05.sql")),
    ];

    for (name, sql) in rollback_queries {
        let st_name = format!("tpch_rb_{name}");

        db.try_execute(&format!(
            "SELECT pgtrickle.create_stream_table('{st_name}', $${sql}$$, NULL, 'IMMEDIATE')"
        )).await.expect("create failed");

        // Baseline
        assert_tpch_invariant(&db, &st_name, sql, name, 0).await.unwrap();

        // Snapshot pre-mutation state
        let cols = /* get user columns */;
        let pre_count: i64 = db.query_scalar(&format!(
            "SELECT count(*) FROM public.{st_name}"
        )).await;

        // RF1 with ROLLBACK
        db.execute("BEGIN").await;
        let next_ok = max_orderkey(&db).await + 1;
        try_apply_rf1(&db, next_ok).await.expect("RF1 failed");
        // ST must have changed (trigger fired)
        let mid_count: i64 = db.query_scalar(&format!(
            "SELECT count(*) FROM public.{st_name}"
        )).await;
        assert_ne!(pre_count, mid_count, "{name}: ST unchanged after RF1 (trigger did not fire)");
        db.execute("ROLLBACK").await;

        // ST must be back to pre-mutation state
        assert_tpch_invariant(&db, &st_name, sql, name, 0).await
            .expect("ST not restored after ROLLBACK");
        let post_count: i64 = db.query_scalar(&format!(
            "SELECT count(*) FROM public.{st_name}"
        )).await;
        assert_eq!(pre_count, post_count, "{name}: ST row count changed after ROLLBACK");

        // Repeat for RF2 (DELETE) and RF3 (UPDATE) ...

        db.try_execute(&format!(
            "SELECT pgtrickle.drop_stream_table('{st_name}')"
        )).await.ok();
    }
}
```

**Note on `BEGIN`/`ROLLBACK` with sqlx:** `E2eDb::execute` runs each
statement as its own implicit transaction. To span a `BEGIN`/`ROLLBACK`
block, use `db.execute("BEGIN")` and `db.execute("ROLLBACK")` as explicit
statement strings (sqlx passes them through to the server). This works
because sqlx's connection pool uses a single connection when `E2eDb`
wraps a single-pool URL.

---

### T4 — `test_tpch_differential_vs_immediate`

**File:** `tests/e2e_tpch_tests.rs`  
**Priority:** Medium — closes the last mode-pair gap.

Mirror of `test_tpch_full_vs_differential`: for each query, create one ST in
DIFFERENTIAL mode and one in IMMEDIATE mode, apply the same RF mutations,
refresh the DIFFERENTIAL one explicitly, and compare the two STs directly.

Key differences from `test_tpch_full_vs_differential`:

- IMMEDIATE ST uses `NULL` schedule.
- After each RF batch: no refresh call for IMMEDIATE (already current); call
  `refresh_stream_table()` for DIFFERENTIAL.
- Assert directly: `DIFFERENTIAL EXCEPT ALL IMMEDIATE` and
  `IMMEDIATE EXCEPT ALL DIFFERENTIAL` must both be empty.
- Only queries that succeed in both modes are included (queries that
  IMMEDIATE can't handle are skipped with a logged reason).

**Table name prefix:** `tpch_di_<qname>` for DIFFERENTIAL, `tpch_ii_<qname>`
for IMMEDIATE — avoids collision with other test prefixes.

**Assertion cadence:** Once per cycle (after RF1+RF2+RF3), not three times
per cycle. The goal is mode agreement, not per-operation trigger validation
(that's `test_tpch_immediate_correctness`).

---

### T5 — `test_tpch_single_row_mutations`

**File:** `tests/e2e_tpch_tests.rs`  
**Priority:** Medium — targets a different code path than batch RF mutations.

**Design:**  
Use a focused subset of 3 queries (q01 aggregate, q06 filter+agg, q03
join+agg) in IMMEDIATE mode. For each query:

1. Create ST.
2. Baseline assertion.
3. Insert exactly 1 order + its lineitems. Assert invariant.
4. Update exactly 1 lineitem field (e.g. `l_discount`). Assert invariant.
5. Delete exactly 1 order + its lineitems. Assert invariant.
6. Drop ST.

**SQL for single-row mutations:**

Inline SQL rather than the RF1/RF2/RF3 files (which are batch-oriented).
Use fixed order keys (e.g. `o_orderkey = 9999991`) that don't collide with
existing data.

```sql
-- single_insert.sql
INSERT INTO orders (o_orderkey, o_custkey, o_orderstatus, ...)
VALUES (9999991, 1, 'O', ...);
INSERT INTO lineitem (l_orderkey, l_linenumber, ...)
VALUES (9999991, 1, ...), (9999991, 2, ...);

-- single_update.sql
UPDATE lineitem SET l_discount = 0.05 WHERE l_orderkey = 9999991;

-- single_delete.sql
DELETE FROM lineitem WHERE l_orderkey = 9999991;
DELETE FROM orders WHERE o_orderkey = 9999991;
```

Store in `tests/tpch/single_row_insert.sql`, `single_row_update.sql`,
`single_row_delete.sql`.

---

### T6 — `test_tpch_dag_chain`

**File:** `tests/e2e_tpch_tests.rs` (or a new `tests/e2e_tpch_dag_tests.rs`)  
**Priority:** Lower — requires verifying DAG refresh interaction, separate
from pure DVM correctness.

**Design:**  
Create a two-level DAG:

- Level 0: `tpch_dag_q01` — q01 (aggregate over `lineitem`), DIFFERENTIAL.
- Level 1: `tpch_dag_derived` — a simple query over `tpch_dag_q01`, e.g.
  `SELECT * FROM tpch_dag_q01 WHERE l_returnflag = 'R'`, DIFFERENTIAL.

Apply RF mutations to `lineitem`. Call `pgtrickle.refresh_stream_table()`
on the level-0 ST, then on the level-1 ST (or use
`pgtrickle.refresh_dag()` if available). Assert both STs match their
respective defining queries.

**Why q01:** It's a pure aggregate with no joins — its downstream behaviour is
predictable and its DVM delta path is well-understood. It isolates the DAG
refresh scheduler from DVM complexity.

**What this covers:**
- `dag.rs` topological sort (dependency ordering).
- CDC trigger fan-out to a stream-table-referencing stream table.
- Two-level refresh ordering correctness.

**Extension:** Once the basic chain passes, add a q03 (join) as a second
level-0 ST with a shared level-1 derived ST. This tests multi-parent DAG
fan-in.

---

## File Changes Summary

| File | Change |
|------|--------|
| `tests/e2e_tpch_tests.rs` | Add T1 (`__pgt_count` check), T2 (skip guard), T3 (`test_tpch_immediate_rollback`), T4 (`test_tpch_differential_vs_immediate`), T5 (`test_tpch_single_row_mutations`) |
| `tests/tpch/single_row_insert.sql` | New — single-row INSERT for T5 |
| `tests/tpch/single_row_update.sql` | New — single-row UPDATE for T5 |
| `tests/tpch/single_row_delete.sql` | New — single-row DELETE for T5 |
| `tests/e2e_tpch_dag_tests.rs` | New (optional, may go in main file) — T6 |

No changes to `src/`, `.github/workflows/ci.yml`, `justfile`, or any SQL
upgrade scripts.

---

## Implementation Order

Recommended sequence — each item is independently shippable:

1. **T1** — `__pgt_count` guard in `assert_tpch_invariant`. Zero risk, pure
   additive. No new tests, but strengthens all existing ones.

2. **T3** — `test_tpch_immediate_rollback`. Highest correctness value.
   Implement `BEGIN`/`ROLLBACK` pattern, populate skip set from first run.

3. **T2** — Skip-set regression guard. Requires T3 first run to populate
   `IMMEDIATE_SKIP_ALLOWLIST`. The `DIFFERENTIAL_SKIP_ALLOWLIST` is
   already known to be empty.

4. **T4** — `test_tpch_differential_vs_immediate`. Depends on T3 to
   understand which queries IMMEDIATE mode can handle.

5. **T5** — `test_tpch_single_row_mutations`. Self-contained; can be
   developed in parallel with T3/T4.

6. **T6** — `test_tpch_dag_chain`. Most complex; depends on having a
   stable picture of which queries work in DIFFERENTIAL mode (already
   established) and understanding of DAG refresh scheduler behaviour.

---

## Acceptance Criteria

| Test | Pass Condition |
|------|---------------|
| T1 (`__pgt_count` guard) | All existing TPC-H tests continue to pass; a query that introduces negative `__pgt_count` now fails fast instead of masking. |
| T2 (skip guard) | A DVM regression that causes a previously-passing query to be skipped is caught and reported by name. |
| T3 (`rollback`) | All 4 representative queries pass all 3 RF-type rollback assertions, or are soft-skipped with a documented reason. |
| T4 (`diff vs imm`) | Queries that succeed in both modes produce identical results for all cycles. |
| T5 (`single row`) | All 3 representative queries pass all 3 single-row mutation assertions in IMMEDIATE mode. |
| T6 (`dag chain`) | Two-level DAG produces correct results after mutations and ordered refresh. |

All new tests are `#[ignore]` and run automatically in CI via the existing
`cargo test --test e2e_tpch_tests -- --ignored` step in the `e2e-tests` job.
No CI configuration changes are needed.
