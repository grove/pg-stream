# PLAN: SQL Gaps — Phase 5

**Status:** Proposed  
**Date:** 2026-02-24  
**Branch:** `main`  
**Scope:** Next priorities for SQL feature coverage — both new implementations and revisiting rejected constructs.  
**Current state:** 872 unit tests, 22 E2E test suites, 25 AggFunc variants, 21 OpTree variants, 20 diff operators. Zero P0 or P1 issues. All remaining gaps are P2+ with clear error messages.

---

## Part A: Top 10 Gaps to Implement Next

These are unimplemented features or correctness gaps ordered by impact-to-effort
ratio. Items higher on the list provide the most value for the least work.

### A1. Non-Deterministic Function Detection

| Field | Value |
|-------|-------|
| **Gap** | Volatile functions (`random()`, `gen_random_uuid()`, `clock_timestamp()`) silently break DIFFERENTIAL delta computation |
| **Current behavior** | No detection — silently accepted, produces phantom changes and broken row hashes |
| **Severity** | **Correctness gap** — not P0 (no wrong SQL generated) but produces wrong *data* |
| **Effort** | 1–2 hours |
| **Impact** | High — prevents silent data corruption for any user using volatile functions |
| **Plan** | `plans/sql/NON_DETERMINISM.md` (415 lines, fully designed) |

**Implementation:**
- Add `lookup_function_volatility()` — SPI query to `pg_proc.provolatile`
- Add recursive `worst_volatility()` Expr tree scanner
- Add `tree_worst_volatility()` OpTree walker
- Enforce at `create_stream_table()` time:
  - DIFFERENTIAL + volatile → reject with clear error
  - DIFFERENTIAL + stable → warn (e.g., `now()` is safe within a single refresh)
  - FULL + volatile → warn
- ~110 lines of new code + 6 E2E tests

**Why #1:** This is the last remaining **silent correctness gap** in the system.
Every other unsupported construct is either working or rejected with a clear
error. Volatile functions are the only case where the system silently accepts a
query that will produce wrong results.

---

### A2. DISTINCT ON Auto-Rewrite

| Field | Value |
|-------|-------|
| **Gap** | `DISTINCT ON (expr)` rejected → user must manually rewrite to `ROW_NUMBER()` |
| **Current behavior** | Rejected with error suggesting DISTINCT or ROW_NUMBER() |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 6–8 hours |
| **Impact** | Medium — common PostgreSQL idiom for "first row per group" |
| **Source** | SQL_GAPS_4.md item S2 |

**Implementation:**
- At parse time, detect `DISTINCT ON` with non-empty `distinctClause`
- Transparently rewrite to a subquery:
  ```sql
  -- Input:
  SELECT DISTINCT ON (customer_id) customer_id, order_id, created_at
  FROM orders ORDER BY customer_id, created_at DESC

  -- Rewrite to:
  SELECT customer_id, order_id, created_at FROM (
    SELECT *, ROW_NUMBER() OVER (
      PARTITION BY customer_id ORDER BY created_at DESC
    ) AS __pgs_rn FROM orders
  ) __pgs_don WHERE __pgs_rn = 1
  ```
- Reuses existing Window + Filter operators — no new OpTree variant needed
- ~150 lines in `parser.rs` + unit tests + E2E tests

**Why #2:** DISTINCT ON is one of the most frequently used PostgreSQL-specific
constructs. The auto-rewrite is clean, well-understood, and reuses existing
infrastructure.

---

### A3. ALL (subquery)

| Field | Value |
|-------|-------|
| **Gap** | `WHERE col > ALL (SELECT ...)` rejected |
| **Current behavior** | Rejected with error suggesting NOT EXISTS rewrite |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 4–6 hours |
| **Impact** | Low-Medium — used in analytical queries |
| **Source** | SQL_GAPS_4.md item E3 |

**Implementation:**
- `ALL (subquery)` is the dual of `ANY (subquery)`:
  `col > ALL(S)` ≡ `NOT EXISTS (SELECT 1 FROM S WHERE NOT (col > S.val))`
- Rewrite `ALL_SUBLINK` to `AntiJoin` with negated condition
- Follows the exact same pattern as `IN/NOT IN → SemiJoin/AntiJoin`
- ~80 lines: parser rewrite + reuse existing AntiJoin operator

**Why #3:** Minimal effort — follows the established AntiJoin pattern exactly.
Covers the last missing subquery expression type.

---

### A4. Regression Aggregates (11 functions)

| Field | Value |
|-------|-------|
| **Gap** | `CORR`, `COVAR_POP`, `COVAR_SAMP`, `REGR_AVGX/AVGY/COUNT/INTERCEPT/R2/SLOPE/SXX/SXY` rejected |
| **Current behavior** | Recognized and rejected with error suggesting FULL mode |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 4–6 hours |
| **Impact** | Low — niche statistical use, but covers 11 gap items at once |
| **Source** | SQL_GAPS_4.md item A3 |

**Implementation:**
- All 11 use the proven group-rescan pattern (copy-paste of existing aggregates)
- Add 11 new `AggFunc` enum variants
- Each returns NULL sentinel on group change → triggers re-aggregation from source
- Mechanical work — no new logic, no new operators
- ~200 lines (enum variants + match arms) + unit tests

**Why #4:** Closes the largest single batch of gap items (11 functions) with
minimal new code. All follow the exact same pattern as STDDEV/VARIANCE.

---

### A5. Mixed UNION / UNION ALL

| Field | Value |
|-------|-------|
| **Gap** | Queries mixing `UNION` and `UNION ALL` rejected |
| **Current behavior** | Rejected with error |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 4–6 hours |
| **Impact** | Low — uncommon pattern, but a completeness gap |
| **Source** | SQL_GAPS_4.md item S3 |

**Implementation:**
- PostgreSQL's parser already produces a nested tree for mixed set ops:
  `A UNION B UNION ALL C` → `SetOperationStmt(UNION ALL, SetOperationStmt(UNION, A, B), C)`
- The DVM parser currently flattens this; instead, respect the nesting
- Each nested `SetOperationStmt` maps to the appropriate operator (UnionAll or
  Intersect/Except with dedup)
- ~100 lines in parser.rs set-operation handling

**Why #5:** Low effort, improves completeness. The fix is mostly about
respecting PostgreSQL's already-correct parse tree structure.

---

### A6. TRUNCATE Capture in CDC

| Field | Value |
|-------|-------|
| **Gap** | `TRUNCATE` on a source table not detected — stream table becomes silently stale |
| **Current behavior** | No detection, no error, stale data |
| **Severity** | **Correctness gap** — similar to A1 |
| **Effort** | 4–6 hours |
| **Impact** | Medium — TRUNCATE is common in ETL workflows |

**Implementation:**
- Add `AFTER TRUNCATE` statement-level trigger alongside the existing row-level
  triggers in `src/cdc.rs`
- The truncate trigger function marks all dependent stream tables for
  reinitialization (`needs_reinit = true`)
- For WAL-mode CDC, logical decoding naturally captures TRUNCATE messages —
  add handling in `src/wal_decoder.rs`
- ~80 lines in `cdc.rs` + ~30 lines in `wal_decoder.rs` + E2E tests

**Why #6:** Second remaining silent correctness gap (after volatile functions).
Simple to implement with `AFTER TRUNCATE` triggers.

---

### A7. GROUPING SETS / CUBE / ROLLUP

| Field | Value |
|-------|-------|
| **Gap** | Advanced aggregation groupings rejected |
| **Current behavior** | Rejected with error suggesting separate STs + UNION ALL |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 10–15 hours |
| **Impact** | Medium — used in reporting and OLAP queries |
| **Source** | SQL_GAPS_4.md item S1 |

**Implementation (preferred: parse-time rewrite):**
- Decompose into multiple GROUP BY queries combined with UNION ALL
- `GROUPING SETS ((a), (b), (a,b))` →
  ```sql
  SELECT a, NULL AS b, SUM(x) FROM t GROUP BY a
  UNION ALL
  SELECT NULL AS a, b, SUM(x) FROM t GROUP BY b
  UNION ALL
  SELECT a, b, SUM(x) FROM t GROUP BY a, b
  ```
- Add `GROUPING()` function results as literal columns in the rewrite
- Reuses existing Aggregate + UnionAll operators
- ~250 lines (parser rewrite + GROUPING() synthesis)
- CUBE and ROLLUP are syntactic sugar generating specific grouping set
  combinations — handled by enumerating them

**Why #7:** High effort but important for analytical workloads. The UNION ALL
rewrite avoids needing a new OpTree variant.

---

### A8. Multiple PARTITION BY in Window Functions

| Field | Value |
|-------|-------|
| **Gap** | Window functions with different `PARTITION BY` clauses rejected |
| **Current behavior** | Rejected with error — must share same PARTITION BY |
| **Severity** | P2 — rejected with clear error |
| **Effort** | 8–10 hours |
| **Impact** | Low — edge case, but blocks legitimate analytics queries |
| **Source** | SQL_GAPS_4.md item S4 |

**Implementation (multi-pass recomputation):**
- Group window functions by their PARTITION BY clause
- For each distinct partitioning, run a separate recomputation pass
- The superset of affected partitions across all passes forms the final delta
- Requires splitting a single Window OpTree node into multiple passes with a
  final join to combine results
- ~200 lines: parser grouping + multi-pass window operator

**Why #8:** Medium-high effort. Legitimate use case but workaround (split into
separate stream tables) exists.

---

### A9. Recursive CTE in DIFFERENTIAL Mode (Incremental Fixpoint)

| Field | Value |
|-------|-------|
| **Gap** | WITH RECURSIVE rejected in DIFFERENTIAL mode → must use FULL |
| **Current behavior** | Rejected with clear error |
| **Severity** | P2 |
| **Effort** | 15–20 hours |
| **Impact** | Medium — recursive CTEs are common for graph traversal, hierarchies |

**Implementation:**
- Currently handled by recomputation-diff (full re-execution + anti-join against
  storage) which is functional but forces FULL mode
- Incremental approach: semi-naive evaluation with Delete-and-Rederive (DRed):
  1. Propagate source deltas through the non-recursive (base) term
  2. Iterate the recursive term with only new rows until fixpoint
  3. For deletions: over-delete, then rederive to restore rows with alternative
     derivations
- Requires monotonicity analysis to determine if convergence is guaranteed
- New operator: `IncrementalRecursiveCte` with iteration loop in delta SQL
- ~400 lines: new operator + monotonicity checker + iteration control

**Why #9:** High effort and complexity, but recursive CTEs are the only major
SQL feature category that is mode-restricted. Enabling them in DIFFERENTIAL mode
would significantly expand the extension's coverage.

---

### A10. Scalar Subquery in WHERE

| Field | Value |
|-------|-------|
| **Gap** | `WHERE col > (SELECT avg(x) FROM t)` rejected in DIFFERENTIAL mode |
| **Current behavior** | Rejected with error suggesting JOIN or CTE rewrite |
| **Severity** | P2 |
| **Effort** | 6–8 hours |
| **Impact** | Low — JOIN/CTE rewrite is straightforward |
| **Source** | SQL_GAPS_4.md item E1 |

**Implementation:**
- Auto-rewrite to a CROSS JOIN with the scalar subquery:
  ```sql
  -- Input:
  SELECT * FROM orders WHERE amount > (SELECT avg(amount) FROM orders)
  -- Rewrite to:
  SELECT o.* FROM orders o
  CROSS JOIN (SELECT avg(amount) AS __pgs_scalar FROM orders) __pgs_sq
  WHERE o.amount > __pgs_sq.__pgs_scalar
  ```
- The CROSS JOIN produces exactly one row from the scalar subquery
- Reuses existing InnerJoin operator for delta computation
- ~120 lines in parser.rs (SubLink detection + rewrite)

**Why #10:** Medium effort. The auto-rewrite approach avoids needing per-row
value-change tracking. Works for the common case of simple scalar comparisons.

---

## Part B: Top 10 Rejected Constructs to Revisit

These are constructs currently rejected with clear error messages that should be
reconsidered for implementation. Ordered by user impact and feasibility.

### B1. DISTINCT ON → Auto-Rewrite to Window Function

| Field | Value |
|-------|-------|
| **Current rejection** | *"DISTINCT ON is not supported. Use DISTINCT or ROW_NUMBER() OVER (...) = 1."* |
| **Recommendation** | **Implement** — auto-rewrite at parse time (same as A2 above) |
| **Effort** | 6–8 hours |
| **User impact** | High — extremely common PostgreSQL idiom |

The ROW_NUMBER() rewrite is clean and mechanical. Users shouldn't need to
manually restructure their queries for this common pattern.

---

### B2. ALL (subquery) → Anti-Join Rewrite

| Field | Value |
|-------|-------|
| **Current rejection** | *"ALL (subquery) is not supported. Use NOT EXISTS with negated condition."* |
| **Recommendation** | **Implement** — rewrite to AntiJoin with negated condition (same as A3 above) |
| **Effort** | 4–6 hours |
| **User impact** | Medium — completes subquery expression coverage |

Follows the exact same pattern as the existing `IN → SemiJoin` rewrite. The
NOT EXISTS alternative works but is unnecessarily verbose for users.

---

### B3. Mixed UNION / UNION ALL → Respect Nested Parse Tree

| Field | Value |
|-------|-------|
| **Current rejection** | *"Mixed UNION / UNION ALL not supported. Use all UNION or all UNION ALL."* |
| **Recommendation** | **Implement** — respect PostgreSQL's nested SetOperationStmt tree (same as A5) |
| **Effort** | 4–6 hours |
| **User impact** | Low-Medium — completeness improvement |

The parser already has the information; it just needs to stop flattening mixed
set operations into a single list.

---

### B4. GROUPING SETS / CUBE / ROLLUP → UNION ALL Decomposition

| Field | Value |
|-------|-------|
| **Current rejection** | *"GROUPING SETS is not supported. Use separate stream tables with UNION ALL, or FULL refresh mode."* |
| **Recommendation** | **Implement** — parse-time UNION ALL rewrite (same as A7) |
| **Effort** | 10–15 hours |
| **User impact** | Medium — important for OLAP and reporting |

The current workaround (manual decomposition) is tedious and error-prone for
CUBE (which generates $2^n$ grouping sets). Automatic decomposition is cleaner.

---

### B5. Recursive CTE in DIFFERENTIAL → Incremental Fixpoint

| Field | Value |
|-------|-------|
| **Current rejection** | *"Recursive CTE is not supported in DIFFERENTIAL mode. Use FULL refresh mode."* |
| **Recommendation** | **Implement for monotone cases** — semi-naive evaluation with DRed (same as A9) |
| **Effort** | 15–20 hours |
| **User impact** | Medium — graph traversal, hierarchies, transitive closure |

At minimum, monotone recursive CTEs (containing only JOINs, UNIONs, filters)
should be allowed. Non-monotone recursive CTEs (containing EXCEPT, aggregates)
can remain rejected or require explicit opt-in.

---

### B6. SubLinks Inside OR → OR-to-UNION Rewrite

| Field | Value |
|-------|-------|
| **Current rejection** | *"Subquery expressions (EXISTS/IN) inside OR conditions are not supported in DIFFERENTIAL mode. Use UNION or separate stream tables."* |
| **Recommendation** | **Implement with caution** — auto-rewrite `WHERE A OR EXISTS(...)` to `UNION` |
| **Effort** | 8–10 hours |
| **User impact** | Low-Medium — uncommon but catches users off guard |

**Rewrite approach:**
```sql
-- Input:
SELECT * FROM t WHERE status = 'active' OR EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id)
-- Rewrite to:
SELECT * FROM t WHERE status = 'active'
UNION
SELECT t.* FROM t JOIN vip ON vip.id = t.id
```

The OR-to-UNION rewrite is well-known from query optimization literature.
Challenge: ensuring the rewrite is equivalent when the OR arms are not
independent (overlapping results handled by UNION dedup).

---

### B7. Scalar Subquery in WHERE → CROSS JOIN Rewrite

| Field | Value |
|-------|-------|
| **Current rejection** | *"Scalar subquery in WHERE is not supported in DIFFERENTIAL mode. Use JOIN or CTE."* |
| **Recommendation** | **Implement** — auto-rewrite to CROSS JOIN (same as A10) |
| **Effort** | 6–8 hours |
| **User impact** | Low-Medium — common pattern in analytical queries |

The CROSS JOIN approach handles the typical case (`WHERE col > (SELECT
avg(...))`) cleanly. More complex correlated scalar subqueries in WHERE remain
harder but are exceedingly rare.

---

### B8. Multiple PARTITION BY → Multi-Pass Recomputation

| Field | Value |
|-------|-------|
| **Current rejection** | *"All window functions must share the same PARTITION BY clause."* |
| **Recommendation** | **Implement** — multi-pass approach (same as A8) |
| **Effort** | 8–10 hours |
| **User impact** | Low — workaround exists (separate stream tables) |

This restriction is more of an implementation limitation than a fundamental
design constraint. Multi-pass recomputation is correct and bounded.

---

### B9. NATURAL JOIN → Catalog-Resolved Rewrite

| Field | Value |
|-------|-------|
| **Current rejection** | *"NATURAL JOIN is not supported. Use explicit JOIN ... ON."* |
| **Recommendation** | **Keep rejection** — NATURAL JOIN is fragile and considered poor practice |
| **Effort** | 6–8 hours (if implemented) |
| **User impact** | Very low — explicit JOINs are universally preferred |

NATURAL JOIN silently changes semantics when columns are added to either table.
The rejection error already suggests the correct alternative. Implementation
would require catalog access at parse time (`pg_attribute` lookup) to resolve
common column names, adding complexity for minimal user value.

**Verdict:** Keep rejection. The error message is helpful and the alternative
is better practice.

---

### B10. Remaining Rejected Aggregates (17 functions)

| Field | Value |
|-------|-------|
| **Current rejection** | *"Aggregate function X() is not supported in DIFFERENTIAL mode. Use FULL refresh mode."* |
| **Recommendation** | **Implement regression aggregates (11)**, keep rejection for hypothetical-set (4) and XMLAGG (1) |
| **Effort** | 4–6 hours for regression; 4–6 hours for hypothetical-set if desired |
| **User impact** | Low — niche use cases |

**Breakdown:**
- **Regression aggregates** (CORR, COVAR_*, REGR_* — 11 functions): Same group-rescan
  pattern as existing aggregates. Implement on demand. (Same as A4.)
- **Hypothetical-set** (RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST as aggregates):
  Almost always used as window functions, not aggregates. Keep rejection.
- **XMLAGG**: Extremely niche. Keep rejection.

---

## Priority Summary

### Recommended Execution Order

| Session | Items | Total Effort | Cumulative Value |
|---------|-------|-------------|------------------|
| **1** | A1 (volatile functions) + A3 (ALL subquery) | 5–8 hours | Closes last silent correctness gap + completes subquery coverage |
| **2** | A2 (DISTINCT ON) | 6–8 hours | Unlocks common PG idiom |
| **3** | A4 (regression aggregates) + A5 (mixed UNION) | 8–12 hours | Covers 12 gap items in one session |
| **4** | A6 (TRUNCATE capture) | 4–6 hours | Closes second correctness gap |
| **5** | A7 (GROUPING SETS) | 10–15 hours | Major OLAP feature |
| **6+** | A8–A10 (multi-PARTITION, recursive CTE, scalar WHERE) | 29–38 hours | Diminishing returns |

### Items to Keep as Rejections

These items are best left rejected with their current error messages:

| Item | Reason |
|------|--------|
| **NATURAL JOIN** | Fragile, poor practice, explicit JOINs are better |
| **LIMIT / OFFSET** | Fundamental design (stream tables are full result sets) |
| **FOR UPDATE / FOR SHARE** | No row-level locking on stream tables |
| **TABLESAMPLE** | Stream tables materialize complete result sets |
| **ROWS FROM() multi-function** | Extremely niche, single SRF covers all practical use |
| **Hypothetical-set aggregates** | Almost always used as window functions |
| **XMLAGG** | Extremely niche |
| **Window functions in expressions** | Architectural constraint; separate column is cleaner |
| **LATERAL with RIGHT/FULL JOIN** | PostgreSQL itself restricts this |

---

## Success Criteria

After Sessions 1–4 (most impactful work):

- [ ] Volatile functions rejected in DIFFERENTIAL mode with clear error
- [ ] Stable functions produce a warning in DIFFERENTIAL mode
- [ ] `DISTINCT ON` auto-rewritten to ROW_NUMBER() window function
- [ ] `ALL (subquery)` supported via AntiJoin rewrite
- [ ] 11 regression aggregates supported in DIFFERENTIAL mode
- [ ] Mixed UNION / UNION ALL works correctly
- [ ] TRUNCATE on source tables triggers reinitialization
- [ ] 36+ AggFunc variants (up from 25)
- [ ] 900+ unit tests (estimated, up from 872)
- [ ] Documentation updated across SQL_REFERENCE, DVM_OPERATORS, README

---

## Historical Progress Summary

| Plan | Sessions | Items Resolved | Test Growth |
|------|----------|---------------|-------------|
| PLAN_SQL_GAPS_1 | 1 | Window detection, CROSS JOIN, FOR UPDATE, report cleanup | 745 → 757 |
| PLAN_SQL_GAPS_2 | 1 | GROUPING SETS P0, GROUP BY hardening, TABLESAMPLE | 745 → 750 |
| PLAN_SQL_GAPS_3 | ~5 | 5 new aggregates + 3 subquery operators | 750 → 809 |
| PLAN_SQL_GAPS_4 | 1 | Report accuracy + 3 ordered-set aggregates | 809 → 826 |
| Hybrid CDC + user triggers + pgs_ rename | ~3 | Hybrid CDC, user triggers, 72-file rename | 826 → 872 |
| **PLAN_SQL_GAPS_5** | **~6** | **Target: volatile detection, DISTINCT ON, ALL subquery, 11 regression aggs, mixed UNION, TRUNCATE** | **872 → 900+** |
