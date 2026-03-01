# PLAN: Close ORDER BY / LIMIT / OFFSET Gaps

**Status:** Not started
**Effort:** ~20–28 hours total (G1–G3 quick wins, G4 main body of work)

---

## Problem Statement

ORDER BY, LIMIT, and OFFSET each interact with pg_trickle's IVM model
differently. The current handling is largely correct, but a codebase-wide
audit identified four gaps:

| # | Gap | Severity | Effort |
|---|-----|----------|--------|
| G1 | No E2E test for `FETCH FIRST N ROWS ONLY` rejection | Low | 30 min |
| G2 | Subquery OFFSET without ORDER BY silently accepted (no warning) | Low | 1–2 h |
| G3 | (Subset of G2) | Low | — |
| G4 | No TopK (ORDER BY + LIMIT) operator for defining queries | Medium | 16–24 h |

G1–G3 are hardening items. G4 is the main feature gap; competitors (Epsio)
support incremental TopK and it is a common dashboard/leaderboard pattern.

---

## Current State (Reference)

### What already works

| Clause | Top-level defining query | Subquery / LATERAL | Internal (refresh engine) |
|---|---|---|---|
| **ORDER BY** | Accepted, silently discarded | Preserved (DISTINCT ON rewrite, window functions, LATERAL, deparse) | MERGE dedup, scan ordering, ordered-set aggregates |
| **LIMIT** | Rejected (`UnsupportedOperator`) | Allowed; warned if no ORDER BY (F13 / G4.2) | Existence checks, adaptive threshold |
| **OFFSET** | Rejected (`UnsupportedOperator`) | Allowed; **no warning** ← gap | Not used |

### Key code locations

- Top-level rejection: `reject_limit_offset()` in `parser.rs` (L5110) +
  Step 7 of `select_stmt_to_tree` (L6881)
- API entry point: `api.rs` (L106)
- Subquery warning: `warn_limit_without_order_in_subqueries()` in `parser.rs` (L5156)
- ORDER BY discard: Step 6 of `select_stmt_to_tree` (L6875)
- DISTINCT ON rewrite: `rewrite_distinct_on()` in `parser.rs` (L3180)
- Deparse round-trip: `deparse_set_operation` (L2654), `deparse_simple_select` (L2765), subquery deparse (L8770)
- MERGE dedup: `refresh.rs` (L519, L1150)
- Scan ordering: `operators/scan.rs` (L210–L315)

---

## Implementation Plan

### Part 1 — Quick Wins (G1–G3)

#### Step 1: E2E test for FETCH FIRST rejection (G1)

PostgreSQL's `FETCH FIRST N ROWS ONLY` sets the same `limitCount` field as
`LIMIT`. The rejection already works; we just need a test to lock it in.

**File:** `tests/e2e_error_tests.rs`

Add a test adjacent to `test_limit_returns_unsupported_error`:

```rust
#[tokio::test]
async fn test_fetch_first_returns_unsupported_error() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE fetch_src (id INT PRIMARY KEY, val INT)").await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('fetch_st', \
             $$ SELECT id, val FROM fetch_src FETCH FIRST 5 ROWS ONLY $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FETCH FIRST should be rejected");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("LIMIT"),
        "Error should mention LIMIT, got: {err_msg}"
    );
}
```

Also add a `FETCH NEXT` variant to cover the alternate keyword:

```rust
#[tokio::test]
async fn test_fetch_next_returns_unsupported_error() {
    let db = E2eDb::new().await.with_extension().await;
    db.execute("CREATE TABLE fetchnext_src (id INT PRIMARY KEY, val INT)").await;

    let result = db
        .try_execute(
            "SELECT pgtrickle.create_stream_table('fetchnext_st', \
             $$ SELECT id, val FROM fetchnext_src FETCH NEXT 3 ROWS ONLY $$, '1m', 'FULL')",
        )
        .await;
    assert!(result.is_err(), "FETCH NEXT should be rejected");
}
```

**Acceptance:** Both tests pass in `just test-e2e`.

---

#### Step 2: Extend subquery warning to OFFSET (G2/G3)

**File:** `src/dvm/parser.rs` — `check_from_item_limit_warning()`

Currently only checks `inner.limitCount`. Extend to also check
`inner.limitOffset`:

```rust
// Current (LIMIT only):
if !inner.limitCount.is_null() {
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner.sortClause) };
    if sort_list.is_empty() {
        // emit warning about LIMIT without ORDER BY
    }
}

// Add below:
if !inner.limitOffset.is_null() {
    let sort_list = unsafe { pgrx::PgList::<pg_sys::Node>::from_pg(inner.sortClause) };
    if sort_list.is_empty() {
        pgrx::warning!(
            "pg_trickle: subquery '{}' uses OFFSET without ORDER BY. \
             This produces non-deterministic results that may differ between \
             FULL and DIFFERENTIAL refresh. Add ORDER BY for deterministic behavior.",
            alias
        );
    }
}
```

Refactor: extract the alias-resolution logic so it is shared between the
LIMIT and OFFSET checks rather than duplicated.

**Tests:**
- Unit test in `parser.rs` `#[cfg(test)]` module (if the function is
  testable without a backend — it requires `raw_parser`, so likely E2E).
- E2E test: subquery with `OFFSET 5` and no ORDER BY → expect WARNING in
  server log. Subquery with `OFFSET 5 ORDER BY id` → no warning.

**Acceptance:** `just lint` clean, new E2E tests pass.

---

### Part 2 — TopK Operator (G4)

#### Background

`ORDER BY expr LIMIT N` in a defining query currently triggers a hard error.
The goal is to accept it and maintain the top-N rows incrementally.

**Competitive context:** Epsio supports incremental TopK. Feldera does not.
The Epsio gap analysis estimates 16–24h for pg_trickle.

**Use cases:**
- Dashboard leaderboards: `SELECT * FROM scores ORDER BY points DESC LIMIT 100`
- Recent-activity feeds: `SELECT * FROM events ORDER BY created_at DESC LIMIT 50`
- Alert thresholds: `SELECT * FROM metrics WHERE val > X ORDER BY val DESC LIMIT 10`

---

#### Alternatives Considered

Five approaches were evaluated. They differ in implementation complexity,
refresh cost, and how well they compose with the existing DVM pipeline.

##### Option A: ROW_NUMBER() window rewrite ❌ rejected

Rewrite the defining query to a window function:
```sql
-- User writes:
SELECT id, score FROM players ORDER BY score DESC LIMIT 10

-- Rewrite to:
SELECT id, score FROM (
  SELECT id, score,
         ROW_NUMBER() OVER (ORDER BY score DESC) AS __pgt_rn
  FROM players
) __pgt_topk WHERE __pgt_rn <= 10
```

| Aspect | Assessment |
|--------|-----------|
| Complexity | Low — reuses existing window operator |
| FULL refresh cost | O(N) — PostgreSQL optimizes the LIMIT in the original query, but the rewrite forces a full window scan: **O(total rows)** |
| DIFF refresh cost | **O(total rows)** — un-partitioned window → any change recomputes the entire ranking over all rows |
| Correctness | Correct for all change types |
| Hidden columns | Requires hiding `__pgt_rn` from user schema |

**Rejected because** the DIFFERENTIAL refresh path is worse than just
re-running the original query with LIMIT. PostgreSQL can execute
`SELECT ... ORDER BY x LIMIT N` with an index scan + early termination in
O(N) or O(N log total). The ROW_NUMBER rewrite forces O(total) on every
single change — it defeats the purpose of incremental maintenance and is
strictly worse than Option B for every table size.

##### Option B: Scoped recomputation ✅ selected

Store the original `ORDER BY ... LIMIT N` query verbatim. On every refresh
(FULL or DIFFERENTIAL), re-execute it against the source tables and MERGE
the result into the stream table.

```
REFRESH (any mode):
  1. Execute: SELECT ... ORDER BY expr LIMIT N  →  current_top_n
  2. MERGE current_top_n INTO stream_table
     WHEN MATCHED AND row disappeared  → DELETE
     WHEN MATCHED AND row changed      → UPDATE
     WHEN NOT MATCHED                  → INSERT
```

| Aspect | Assessment |
|--------|-----------|
| Complexity | Low — no query rewrite, no new operator, no hidden columns |
| FULL refresh cost | O(N log total) with index, O(total log total) without |
| DIFF refresh cost | O(N log total) — same query, but only runs when changes exist |
| Correctness | Trivially correct — the source query is the definition of truth |
| Composability | Works with GROUP BY, DISTINCT, JOIN, subqueries — anything PostgreSQL accepts |

**Why this is the right first approach:**
- The defining query itself is the optimal way to compute the top-N.
  PostgreSQL's planner already knows how to execute `ORDER BY ... LIMIT`
  efficiently (top-N heapsort, index scan with early stop).
- No query rewrite means no hidden columns, no interaction with DISTINCT ON
  or window rewrites, and no risk of deparse round-trip bugs.
- The MERGE handles all change types (inserts, deletes, updates) uniformly.
- The existing change-detection check ("are there any changes?") still runs
  first, so unchanged sources skip the recomputation entirely.
- Implementation effort is significantly lower than all other options.

**Trade-off:** refresh cost is O(N log total) per cycle, not O(delta). This
is acceptable for the typical TopK use case (small N, indexed sort column).
For N=100 on a 10M-row table with a btree index on the sort column,
PostgreSQL executes the query in ~1ms.

##### Option C: Boundary-tracked recomputation (future optimization)

Enhancement over Option B: store the sort-key value of the Nth row (the
"boundary"). Before re-executing the query, check whether any changed row
crosses the boundary.

```
REFRESH:
  1. Fetch boundary_value from metadata
  2. Check: do any change-buffer rows have sort_key that could enter/exit top-N?
     - INSERT with sort_key better than boundary → yes
     - DELETE of a row in top-N → yes
     - UPDATE that moves sort_key across boundary → yes
  3. If no → skip refresh (zero cost)
  4. If yes → execute full query as in Option B, update boundary
```

| Aspect | Assessment |
|--------|-----------|
| Complexity | Medium — boundary storage, delta inspection logic |
| DIFF refresh cost | O(delta) for the check; O(N log total) only when boundary is crossed |
| Amortized cost | Excellent for high-throughput tables where most changes are outside the top-N |

**Deferred to a follow-up.** Option B must exist first. Boundary tracking
is a pure optimization layered on top — it doesn't change the API, storage,
or correctness model.

##### Option D: Dedicated TopK operator with overflow buffer

Maintain top-N + overflow buffer (top-M, where M > N) in storage. Apply
deltas directly: insert into overflow if sort_key qualifies, evict from
bottom, promote from overflow when top-N rows are deleted.

| Aspect | Assessment |
|--------|-----------|
| Complexity | **High** — new DVM operator, overflow sizing, eviction logic, buffer maintenance |
| DIFF refresh cost | O(N + delta) steady-state; O(total) when overflow is exhausted |
| Correctness | Subtle edge cases: overflow exhaustion, ties, multi-column sort keys |

**Deferred.** Only justified if Option B+C prove insufficient for
real-world workloads. The overflow buffer approach is well-studied in the
streaming database literature but adds significant complexity for a
marginal improvement over C.

##### Option E: Status quo (reject, read-time LIMIT) ❌ rejected

Keep rejecting `LIMIT` in defining queries. Users apply LIMIT when querying.

**Rejected because** it stores the full result set (significant storage
overhead for large tables) and doesn't match user expectations. Competitors
support TopK.

---

#### Decision: Option B — Scoped Recomputation

The simplest correct approach. No query rewriting, no hidden columns, no new
operators. Leans on PostgreSQL's own query optimizer for efficient execution.
Options C and D are documented as future optimizations if profiling shows
the per-refresh cost is a bottleneck.

---

#### Step 3: Parse and detect TopK pattern

**File:** `src/dvm/parser.rs`

Add a function `detect_topk_pattern()`:

```rust
/// Detect ORDER BY + LIMIT (TopK) in a defining query.
///
/// Returns `Some((order_by_sql, limit_value))` if:
/// - Top-level SELECT has both sortClause and limitCount
/// - No limitOffset (OFFSET is not supported for TopK)
/// - No set operations (UNION etc.)
/// - limitCount is a constant integer (not a subquery)
///
/// Returns `None` if the query does not match the TopK pattern.
pub fn detect_topk_pattern(query: &str) -> Result<Option<TopKInfo>, PgTrickleError> {
    // Parse query, check top-level SelectStmt fields.
}

pub struct TopKInfo {
    /// The deparsed ORDER BY clause (e.g., "score DESC, name ASC")
    pub order_by_sql: String,
    /// The LIMIT value as an integer
    pub limit_value: i64,
    /// The defining query with ORDER BY and LIMIT stripped
    pub base_query: String,
}
```

**Validation rules:**
- `LIMIT` without `ORDER BY` → reject (existing behavior, unchanged)
- `ORDER BY` without `LIMIT` → accept and discard ORDER BY (existing behavior)
- `ORDER BY` + `LIMIT` → TopK pattern (new)
- `ORDER BY` + `LIMIT` + `OFFSET` → reject with error:
  *"OFFSET is not supported with LIMIT in defining queries. Use LIMIT alone
  for TopK patterns, and apply OFFSET when querying the stream table."*
- `LIMIT ALL` → no TopK (equivalent to no LIMIT)
- `LIMIT 0` → accept, produces empty stream table
- `LIMIT (SELECT ...)` → reject (require constant integer)
- `FETCH FIRST N ROWS ONLY` → same as `LIMIT N` (handled at parse level)

---

#### Step 4: Store TopK metadata

**File:** `src/catalog.rs`

Extend the `pgtrickle.pgt_stream_tables` catalog with two nullable columns:

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN pgt_topk_limit   INT,        -- NULL = not a TopK table
  ADD COLUMN pgt_topk_order_by TEXT;       -- deparsed ORDER BY clause
```

When `detect_topk_pattern()` returns `Some(info)`, store `limit_value` and
`order_by_sql` in the catalog row. The `pgt_defining_query` stores the
**base query** (without ORDER BY / LIMIT) for use in non-TopK code paths
(dependency analysis, view inlining, etc.).

The original user-supplied query (with ORDER BY + LIMIT) is reconstructed
at refresh time as: `{base_query} ORDER BY {order_by_sql} LIMIT {limit_value}`.

---

#### Step 5: TopK-aware refresh path

**File:** `src/refresh.rs`

When a stream table has `pgt_topk_limit IS NOT NULL`:

**FULL refresh** — replaces the current TRUNCATE + INSERT path:
```sql
-- 1. Compute current top-N
CREATE TEMP TABLE __pgt_topk_result AS
  SELECT *, __pgt_hash_row(...) AS __pgt_row_id
  FROM ({base_query} ORDER BY {order_by} LIMIT {N}) __pgt_src;

-- 2. MERGE into stream table
MERGE INTO {stream_table} AS st
USING __pgt_topk_result AS src
ON st.__pgt_row_id = src.__pgt_row_id
WHEN MATCHED AND (is_distinct_from_check) THEN
  UPDATE SET col1 = src.col1, ...
WHEN NOT MATCHED THEN
  INSERT (__pgt_row_id, col1, ...) VALUES (src.__pgt_row_id, src.col1, ...)
WHEN NOT MATCHED BY SOURCE THEN
  DELETE;
```

**DIFFERENTIAL refresh** — same logic, with change-detection gate:
```
1. Check change buffers for any changes (existing infra)
2. If no changes → skip (zero cost)
3. If changes exist → execute FULL refresh path above
4. Truncate change buffers (existing infra)
```

Note: for TopK tables, DIFFERENTIAL mode doesn't attempt delta computation
through the DVM pipeline. It uses the source query directly. The change
buffer check provides the only "incremental" optimization — skipping
refresh entirely when nothing changed. This is a deliberate simplification;
Option C (boundary tracking) adds finer-grained skipping as a future layer.

**FULL mode** — standard behavior (always recompute, no change-buffer check).

---

#### Step 6: DVM pipeline bypass

**File:** `src/api.rs`, `src/dvm/parser.rs`

TopK tables do **not** go through the DVM tree construction or delta SQL
generation. At creation time:

```
1. (existing) Rewrite DISTINCT ON
2. (existing) Rewrite multi-partition windows
3. (NEW) detect_topk_pattern()
4. If TopK detected:
   a. Store base_query, order_by, limit in catalog
   b. Skip DVM parse / tree construction for the ORDER BY + LIMIT part
   c. Validate base_query via LIMIT 0 probe (existing)
   d. reject_limit_offset on base_query (should pass — LIMIT is stripped)
5. If not TopK:
   a. (existing) reject_limit_offset
   b. (existing) full DVM pipeline
```

This avoids the trap of trying to make the DVM differentiation engine
understand LIMIT semantics — which is fundamentally incompatible with
delta-based reasoning (the boundary between "in" and "out" of the LIMIT
depends on the global sort order, not local deltas).

---

#### Step 7: Tests

**E2E tests** (`tests/e2e_topk_tests.rs` — new file):

| Test | Scenario |
|------|----------|
| `test_topk_basic_create_and_query` | `ORDER BY score DESC LIMIT 5` — verify 5 rows stored |
| `test_topk_differential_insert` | Insert a row that enters the top-N — verify it appears after refresh |
| `test_topk_differential_eviction` | Insert a row that bumps the last row out of top-N |
| `test_topk_differential_delete` | Delete a top-N row — verify a new row is promoted |
| `test_topk_differential_update` | Update a row's sort key so it leaves/enters top-N |
| `test_topk_no_change_skips_refresh` | No source changes → stream table untouched (change gate) |
| `test_topk_full_refresh` | FULL refresh produces same result as DIFFERENTIAL |
| `test_topk_limit_all_accepted` | `LIMIT ALL` → no TopK, full result set |
| `test_topk_limit_zero` | `LIMIT 0` → empty stream table |
| `test_topk_with_offset_rejected` | `LIMIT 10 OFFSET 5` → error |
| `test_topk_non_constant_limit_rejected` | `LIMIT (SELECT count(*) FROM t)` → error |
| `test_topk_limit_without_order_by_rejected` | `LIMIT 5` without ORDER BY → existing error |
| `test_topk_with_group_by` | `GROUP BY region ORDER BY SUM(x) DESC LIMIT 5` |
| `test_topk_with_join` | Join query with ORDER BY + LIMIT |
| `test_topk_with_where` | Filtered query with ORDER BY + LIMIT |
| `test_topk_fetch_first_syntax` | `FETCH FIRST 5 ROWS ONLY` detected as TopK |
| `test_topk_catalog_metadata` | Verify `pgt_topk_limit` and `pgt_topk_order_by` stored correctly |
| `test_topk_alter_unsupported` | `alter_stream_table` on a TopK table → clear error or documented behavior |

**Unit tests** in `parser.rs`:
- `detect_topk_pattern` returns `Some` / `None` for various query shapes
- Boundary cases: LIMIT ALL, LIMIT 0, LIMIT with expression, OFFSET present

---

### Part 3 — Documentation Updates

#### Step 8: Update SQL Reference

**File:** `docs/SQL_REFERENCE.md`

Change the bullet under `create_stream_table`:
```
- **LIMIT / OFFSET** are rejected — stream tables materialize the full result set.
```
to:
```
- **ORDER BY + LIMIT** (TopK) is supported — the stream table maintains the
  top N rows. On each refresh, the defining query is re-executed and the
  result is merged into the stream table.
- **LIMIT** without **ORDER BY** is rejected — without ordering, the "top N"
  concept is undefined.
- **OFFSET** is rejected in defining queries (apply when querying).
- TopK stream tables in DIFFERENTIAL mode skip refresh when no source changes
  are detected, but do not perform delta-based incremental computation — the
  full ORDER BY + LIMIT query is re-executed when changes exist.
```

#### Step 9: Update FAQ

**File:** `docs/FAQ.md`

Rewrite the "Why is LIMIT / OFFSET rejected?" section to explain that
`ORDER BY + LIMIT` is now supported as TopK, while standalone LIMIT and
OFFSET remain rejected. Add a note about performance: works best with an
index on the sort column. Keep the rewrite guidance for OFFSET.

#### Step 10: Update CHANGELOG

Add entry under the next version documenting TopK support.

---

### Part 4 — Fix Workarounds in Other Plans

Once TopK is implemented, the following workarounds across the codebase and
planning documents should be revisited:

#### TPC-H Test Suite — 5 queries with LIMIT stripped

**Source:** [PLAN_TEST_SUITE_TPC_H.md](../testing/PLAN_TEST_SUITE_TPC_H.md)
§ "SQL Workarounds Applied" and
[PLAN_TEST_SUITES.md](../testing/PLAN_TEST_SUITES.md) § "Suite 1: TPC-H"

Five TPC-H queries had `ORDER BY ... LIMIT N` removed because top-level
LIMIT is rejected by the parser. All five are standard TopK patterns:

| Query | Original clause (TPC-H spec) | What was stripped |
|-------|------------------------------|-------------------|
| Q2 — Min Cost Supplier | `ORDER BY s_acctbal DESC, n_name, s_name, p_partkey LIMIT 100` | ORDER BY + LIMIT |
| Q3 — Shipping Priority | `ORDER BY revenue DESC, o_orderdate LIMIT 10` | ORDER BY + LIMIT |
| Q10 — Returned Items | `ORDER BY revenue DESC LIMIT 20` | ORDER BY + LIMIT |
| Q18 — Large Volume Customer | `ORDER BY o_totalprice DESC, o_orderdate LIMIT 100` | ORDER BY + LIMIT |
| Q21 — Suppliers Waiting | `ORDER BY numwait DESC, s_name LIMIT 100` | ORDER BY + LIMIT |

**Action items:**
- [ ] Restore `ORDER BY ... LIMIT N` to `tests/tpch/queries/q02.sql`,
  `q03.sql`, `q10.sql`, `q18.sql`, `q21.sql`
- [ ] Remove the `-- LIMIT removed (unsupported by pg_trickle)` comments
- [ ] Update the "Modifications Applied" table in
  [PLAN_TEST_SUITE_TPC_H.md](../testing/PLAN_TEST_SUITE_TPC_H.md):
  remove the "Remove LIMIT" row
- [ ] Update [PLAN_TEST_SUITES.md](../testing/PLAN_TEST_SUITES.md):
  move Q2, Q3, Q10, Q18, Q21 from "Remove LIMIT" to "Works as-is" and
  change the feature matrix row from ❌ to ✅
- [ ] Verify all 5 queries create successfully as TopK stream tables and
  pass the existing mutation cycles

#### TPC-H Test Suite — query files with ORDER BY stripped

All 22 TPC-H query files had `ORDER BY` removed. Since top-level ORDER BY
is silently discarded this was cosmetic, not functional. After TopK lands:
- Queries that *also* had LIMIT (Q2, Q3, Q10, Q18, Q21) **must** have
  ORDER BY restored — it's essential for TopK semantics.
- Queries with ORDER BY only (no LIMIT) can optionally have ORDER BY
  restored for spec-faithfulness — it will continue to be silently
  discarded.

#### TPC-H Test Suites plan — feature coverage table

**Source:** [PLAN_TEST_SUITES.md](../testing/PLAN_TEST_SUITES.md) § "SQL
Feature Coverage by TPC-H"

Update the row:
```
| LIMIT | Q2,Q3,Q10,Q18,Q21 | ❌ Rejected — must remove |
```
to:
```
| LIMIT (TopK) | Q2,Q3,Q10,Q18,Q21 | ✅ Supported (ORDER BY + LIMIT) |
```

#### Epsio gap analysis — competitive gap closed

**Source:** [GAP_ANALYSIS_EPSIO.md](../ecosystem/GAP_ANALYSIS_EPSIO.md)

Update the comparison table row:
```
| ORDER BY + LIMIT (TopK) | ✅ | ❌ | Epsio |
```
to:
```
| ORDER BY + LIMIT (TopK) | ✅ | ✅ | Tie |
```

And remove TopK from the "Gap for pg_trickle" callout and the
recommendations table.

#### SQL gap analyses

**Sources:**
- [GAP_SQL_PHASE_4.md](GAP_SQL_PHASE_4.md): `LIMIT / OFFSET — Rejected`
- [GAP_SQL_PHASE_6.md](GAP_SQL_PHASE_6.md): `LIMIT / OFFSET / FETCH FIRST —
  Stream tables materialize full result sets`
- [GAP_SQL_PHASE_7.md](GAP_SQL_PHASE_7.md): `LIMIT / OFFSET / FETCH FIRST —
  Rejected — stream tables need all rows`
- [GAP_SQL_OVERVIEW.md](GAP_SQL_OVERVIEW.md): LIMIT example showing error

Update all four to distinguish between:
- `ORDER BY + LIMIT` → ✅ supported (TopK)
- `LIMIT` without `ORDER BY` → ❌ rejected
- `OFFSET` → ❌ rejected

#### pg_ivm comparison

**Source:** [GAP_PG_IVM_COMPARISON.md](../ecosystem/GAP_PG_IVM_COMPARISON.md)

Update the limitations line:
```
- `LIMIT` / `OFFSET` not supported in DIFFERENTIAL mode.
```
to reflect that TopK (`ORDER BY + LIMIT`) is now supported.

#### LATERAL joins plan

**Source:** [PLAN_LATERAL_JOINS.md](PLAN_LATERAL_JOINS.md)

The note that ORDER BY + LIMIT "are normally rejected for stream tables"
should be updated to clarify that they are rejected only at the top level
when LIMIT appears without ORDER BY; ORDER BY + LIMIT (TopK) is now
supported.

---

## Task Checklist

- [ ] **G1** — E2E test: `FETCH FIRST` / `FETCH NEXT` rejection (Step 1)
- [ ] **G2** — Extend subquery warning to OFFSET (Step 2)
- [ ] **G4** — `detect_topk_pattern()` + `TopKInfo` struct (Step 3)
- [ ] **G4** — Catalog schema: `pgt_topk_limit`, `pgt_topk_order_by` (Step 4)
- [ ] **G4** — TopK-aware refresh path in `refresh.rs` (Step 5)
- [ ] **G4** — DVM pipeline bypass in `api.rs` (Step 6)
- [ ] **G4** — E2E + unit tests for TopK (Step 7)
- [ ] Docs — SQL Reference, FAQ, CHANGELOG (Steps 8–10)
- [ ] **TPC-H** — Restore ORDER BY + LIMIT in 5 query files (Part 4)
- [ ] **TPC-H** — Update PLAN_TEST_SUITE_TPC_H.md workaround tables (Part 4)
- [ ] **TPC-H** — Update PLAN_TEST_SUITES.md tables + feature matrix (Part 4)
- [ ] **Gap analyses** — Update GAP_SQL_PHASE_4/6/7, GAP_SQL_OVERVIEW (Part 4)
- [ ] **Ecosystem** — Update GAP_ANALYSIS_EPSIO.md, GAP_PG_IVM_COMPARISON.md (Part 4)
- [ ] `just fmt && just lint` clean
- [ ] `just test-e2e` green
- [ ] `just test-tpch` green (with restored LIMIT queries)

---

## Risks and Open Questions

1. **Refresh cost for large tables without an index.** Without a btree index
   on the sort column, PostgreSQL must sort the entire table on each refresh:
   O(total log total). Mitigation: document that TopK stream tables perform
   best with an index on the ORDER BY column(s). Consider emitting a
   `NOTICE` at creation time if no suitable index is detected.

2. **LIMIT with subquery expression.** Should `LIMIT (SELECT ...)` be
   supported? Decision: no — require a constant integer. Dynamic limits
   would need re-evaluation on every refresh, complicating caching and
   catalog storage.

3. **Interaction with DISTINCT ON + LIMIT.** `SELECT DISTINCT ON (x) ...
   ORDER BY x, y LIMIT 5` — the DISTINCT ON rewrite runs first (producing
   a ROW_NUMBER subquery). TopK detection then sees the rewritten query,
   which no longer has a top-level LIMIT. Two options: (a) detect TopK
   *before* DISTINCT ON rewrite, (b) detect it on the original query text.
   Need to decide ordering. Leaning toward (a): detect TopK first, strip
   LIMIT, then let DISTINCT ON rewrite proceed on the base query.

4. **`alter_stream_table` behavior.** Can a user change the LIMIT value on
   an existing TopK table? Can they remove the LIMIT? This needs API design.
   Initial proposal: `alter_stream_table` with a new defining query
   re-detects TopK; changing between TopK and non-TopK forces a full refresh.

5. **Monitoring / observability.** TopK tables should be identifiable in
   `pgtrickle.stream_tables_info()` output. Expose `topk_limit` and
   `topk_order_by` in the monitoring view.

6. **Future: Option C (boundary tracking).** When profiling shows that
   most refreshes recompute unnecessarily (changes exist but don't affect
   the top-N), implement boundary tracking as a transparent optimization.
   This is backward-compatible: no API or schema changes, just a smarter
   skip decision in the refresh path.
