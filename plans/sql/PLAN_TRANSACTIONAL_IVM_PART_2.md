# Plan: Expanding SQL Coverage in Trigger-Based CDC Mode (Part 2)

**Date:** 2026-03-03  
**Status:** PROPOSED  
**Branch:** TBD  
**Scope:** Limitations of `pg_trickle.cdc_mode = 'trigger'` (the default),
and a phased implementation plan to maximise the SQL surface area supported
in DIFFERENTIAL mode with trigger-based change capture.  
**Predecessor:** [PLAN_TRANSACTIONAL_IVM.md](PLAN_TRANSACTIONAL_IVM.md) (IMMEDIATE mode — Phases 1–4)  
**References:** [GAP_SQL_OVERVIEW.md](GAP_SQL_OVERVIEW.md) · [GAP_SQL_PHASE_4.md](GAP_SQL_PHASE_4.md) · [REPORT_TRIGGERS_VS_REPLICATION.md](REPORT_TRIGGERS_VS_REPLICATION.md)

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Current Trigger-Mode Limitations](#2-current-trigger-mode-limitations)
3. [Remaining SQL Gaps in DIFFERENTIAL Mode](#3-remaining-sql-gaps-in-differential-mode)
4. [Implementation Plan — Phase 1: Auto-Rewrite Completions](#4-implementation-plan--phase-1-auto-rewrite-completions)
5. [Implementation Plan — Phase 2: New DVM Operators](#5-implementation-plan--phase-2-new-dvm-operators)
6. [Implementation Plan — Phase 3: Trigger-Level Optimisations](#6-implementation-plan--phase-3-trigger-level-optimisations)
7. [Implementation Plan — Phase 4: Remaining Aggregate Coverage](#7-implementation-plan--phase-4-remaining-aggregate-coverage)
8. [Implementation Plan — Phase 5: IMMEDIATE Mode Parity](#8-implementation-plan--phase-5-immediate-mode-parity)
9. [Risk Assessment](#9-risk-assessment)
10. [Success Criteria](#10-success-criteria)
11. [Prioritised Execution Order](#11-prioritised-execution-order)
12. [ADR Reference](#12-adr-reference)

---

## 1. Executive Summary

pg_trickle defaults to `pg_trickle.cdc_mode = 'trigger'`, which uses
row-level AFTER triggers on source tables to capture INSERT/UPDATE/DELETE
changes into per-source buffer tables. This mode works everywhere — no
`wal_level = logical`, no replication slots, no REPLICA IDENTITY
configuration — but it imposes a set of inherent limitations on the change
capture layer, and the DVM engine that consumes those changes still has
several SQL constructs that are rejected or handled sub-optimally.

This plan catalogues every limitation, classifies each by root cause (trigger
architecture vs DVM engine vs auto-rewrite gap), and proposes a phased
implementation plan to close as many gaps as possible **without** requiring
users to switch to WAL-based CDC.

**Current state (ground truth):**

- 872 unit tests, 22 E2E test suites passing
- 25 aggregate functions in DIFFERENTIAL mode
- 21 OpTree variants, 20 diff operators
- 5 auto-rewrite passes (view inlining, DISTINCT ON, GROUPING SETS, scalar
  subquery in WHERE, SubLinks in OR)
- NATURAL JOIN fully resolved at parse time
- TRUNCATE detected via statement-level trigger → automatic FULL refresh
  fallback

**After this plan (target):**

- 900+ unit tests
- 36+ aggregate functions in DIFFERENTIAL mode (11 regression + hypothetical)
- Multiple PARTITION BY resolved via query-level rewrite
- Mixed UNION / UNION ALL handled natively
- ALL (subquery) operator
- Column-level change compression reducing trigger overhead by 30–60% for
  UPDATE-heavy workloads on wide tables
- Batched trigger writes for bulk DML
- IMMEDIATE mode supports recursive CTEs and TopK

---

## 2. Current Trigger-Mode Limitations

The following limitations are **inherent to the trigger-based CDC
architecture**. They apply regardless of what the DVM engine supports.
Each is annotated with its user-visible impact and whether it can be
mitigated within trigger mode.

### 2.1 Write-Path Overhead (Synchronous)

| Aspect | Detail |
|--------|--------|
| **Root cause** | Every INSERT/UPDATE/DELETE on a tracked source table runs a PL/pgSQL trigger function that writes a row into the change buffer table and updates a B-tree index — all inside the application's committing transaction. |
| **Cost** | ~2–4 μs per narrow INSERT, ~5–15 μs per wide UPDATE. 2–3× write amplification (source WAL + buffer heap write + buffer WAL + index WAL). |
| **Throughput ceiling** | Estimated ~5,000 writes/sec per source before trigger overhead dominates. |
| **User impact** | Application DML latency increases by the trigger cost. Under high load, trigger execution becomes the bottleneck. |
| **Mitigable?** | Partially — see Phase 3 (column-level change compression, batched writes). Fully solvable only by switching to WAL-based CDC (`pg_trickle.cdc_mode = 'auto'`). |

### 2.2 TRUNCATE Causes Full-Refresh Fallback

| Aspect | Detail |
|--------|--------|
| **Root cause** | PostgreSQL does not fire row-level triggers for TRUNCATE. A separate statement-level AFTER TRUNCATE trigger writes a marker row (`action = 'T'`) into the change buffer. |
| **Current handling** | The refresh engine detects the 'T' marker and falls back to a FULL refresh (TRUNCATE stream table + repopulate from defining query). |
| **User impact** | ETL patterns that TRUNCATE-and-reload source tables trigger expensive full refreshes instead of incremental deltas. For large stream tables this can take minutes. |
| **Mitigable?** | Partially — see Phase 3 (incremental TRUNCATE via negation delta). Fully solvable via WAL mode (native TRUNCATE capture). |

### 2.3 Per-Column Capture Overhead for Wide Tables

| Aspect | Detail |
|--------|--------|
| **Root cause** | The CDC trigger copies **every column** of the NEW and OLD records into the buffer table, regardless of which columns actually changed. An UPDATE that modifies 1 column in a 50-column table still writes 100 column values (50 new + 50 old). |
| **User impact** | Buffer table storage grows proportional to table width × DML volume. Wide tables produce large buffer entries, increasing I/O and VACUUM cost. |
| **Mitigable?** | Yes — see Phase 3 (column-level change detection, only store modified columns). |

### 2.4 Buffer Table Storage and Vacuum Pressure

| Aspect | Detail |
|--------|--------|
| **Root cause** | Change buffers are regular heap tables. Between refresh cycles, dead tuples accumulate (each refresh deletes consumed rows). Autovacuum must reclaim this space. |
| **User impact** | High-write sources produce buffer table bloat. Short refresh schedules (10s) mitigate this but increase scheduler overhead. |
| **Mitigable?** | Yes — see Phase 3 (buffer table partitioning by LSN range, TRUNCATE-based cleanup). |

### 2.5 Schema Evolution Complexity

| Aspect | Detail |
|--------|--------|
| **Root cause** | `ALTER TABLE ADD/DROP/RENAME COLUMN` on a source table requires rebuilding the PL/pgSQL trigger function and adding/dropping columns on the buffer table. DDL event triggers handle this, but the buffer table schema must be kept in sync. |
| **User impact** | ALTER TABLE succeeds but the next refresh may fail if the buffer schema is stale. The `needs_reinit` flag forces a full refresh after schema changes. |
| **Mitigable?** | Already handled (DDL event hooks rebuild trigger in-place). Further improvement: online trigger rebuild without full reinit — see Phase 3. |

### 2.6 Self-Join Change Correlation

| Aspect | Detail |
|--------|--------|
| **Root cause** | When a defining query joins a table to itself (`FROM orders o1 JOIN orders o2 ON ...`), both sides share the same change buffer. The delta must correlate changes from a single buffer against the current state of both aliases. |
| **Current handling** | The DVM engine already handles this correctly — each `Scan` node produces its own delta CTE from the same buffer, and the join operator combines them per the differential calculus rules. |
| **User impact** | Works correctly but:  the same change buffer is scanned twice, doubling read I/O. |
| **Mitigable?** | Yes — CTE materialisation in the delta SQL already prevents double-scan at the PostgreSQL level. No further optimisation needed. |

### 2.7 Concurrent Transaction LSN Interleaving

| Aspect | Detail |
|--------|--------|
| **Root cause** | Each trigger invocation calls `pg_current_wal_insert_lsn()` to timestamp the change. Under concurrent transactions, LSN values interleave — two transactions' changes may share overlapping LSN ranges. |
| **Current handling** | The scan delta uses a closed-open LSN range (`lsn > prev_frontier AND lsn <= new_frontier`) with a net-effect window function (`ROW_NUMBER() OVER (PARTITION BY pk_hash ORDER BY change_id DESC)`) to collapse multiple changes per PK into one net action. |
| **User impact** | Correct but: concurrent heavy writes can produce large net-effect windows, increasing delta computation cost. |
| **Mitigable?** | Already handled correctly. Further improvement via change_id-based sequencing (Phase 3). |

---

## 3. Remaining SQL Gaps in DIFFERENTIAL Mode

The following constructs are currently **rejected with clear error messages**
in DIFFERENTIAL mode. Each could be implemented to expand the SQL surface
area available to trigger-mode users.

### 3.1 Summary of All Remaining Gaps

| ID | Construct | Current Status | Root Cause | Proposed Fix | Phase |
|----|-----------|----------------|------------|-------------|-------|
| **G1** | Mixed UNION / UNION ALL | Rejected | No per-arm dedup flag in OpTree | Add `dedup` flag per UNION arm | 1 |
| **G2** | Multiple PARTITION BY in one query | Rejected | Single-pass partition recompute | Auto-rewrite to nested subqueries | 1 |
| **G3** | ALL (subquery) | Rejected | Not implemented | New AntiJoin variant with universal quantification | 2 |
| **G4** | SubLinks inside deeply nested OR | Rejected | Auto-rewrite handles simple cases; deeply nested patterns escape | Extend `rewrite_sublinks_in_or` to handle nested boolean trees | 2 |
| **G5** | ROWS FROM() with multiple functions | Rejected | Parser only handles single SRF | Extend `LateralFunction` to zip multiple SRFs | 2 |
| **G6** | LATERAL with RIGHT/FULL JOIN | Rejected | Delta correctness not validated | Implement NULL-padded LATERAL delta for RIGHT/FULL | 2 |
| **G7** | Regression aggregates (11 functions) | Rejected | Not implemented | Group-rescan (same pattern as existing aggregates) | 4 |
| **G8** | Hypothetical-set aggregates (4 funcs) | Rejected | Not implemented | Group-rescan | 4 |
| **G9** | XMLAGG | Rejected | Not implemented | Group-rescan | 4 |
| **G10** | Recursive CTEs in IMMEDIATE mode | Rejected | Fixpoint iteration not validated with transition tables | Validate and enable | 5 |
| **G11** | TopK in IMMEDIATE mode | Rejected | Scoped recompute is a deferred/full pattern | Statement-level TopK maintenance | 5 |
| **G12** | Window functions nested in expressions | Rejected | `expr_contains_window_function()` detects and rejects | Lift window to separate column, rewrite outer expr | 1 |

### 3.2 Items That Are NOT Gaps (Correctly Handled)

These are sometimes perceived as gaps but are actually fully supported or
intentionally not applicable:

| Construct | Status | Notes |
|-----------|--------|-------|
| NATURAL JOIN | ✅ Fully supported | Common columns resolved at parse time (S9) |
| DISTINCT ON | ✅ Auto-rewritten | ROW_NUMBER() OVER (PARTITION BY … ORDER BY …) = 1 |
| GROUPING SETS / CUBE / ROLLUP | ✅ Auto-rewritten | Decomposed into UNION ALL of separate GROUP BY queries |
| Scalar subquery in WHERE | ✅ Auto-rewritten | Rewritten to CROSS JOIN |
| SubLinks in simple OR | ✅ Auto-rewritten | Decomposed into UNION branches |
| Views in FROM | ✅ Auto-rewritten | Inlined to subqueries (fixpoint, max depth 10) |
| Recursive CTEs in DIFFERENTIAL | ✅ Supported | Semi-naive + DRed + recomputation strategies |
| FOR UPDATE / FOR SHARE | Rejected | Not applicable — stream tables don't use row locking |
| OFFSET | Rejected | Not applicable — stream tables are full result sets |
| TABLESAMPLE | Rejected | Not applicable — non-deterministic by design |

---

## 4. Implementation Plan — Phase 1: Auto-Rewrite Completions

**Goal:** Close 3 gaps by extending the auto-rewrite pipeline. No new DVM
operators needed — these transform unsupported patterns into already-supported
SQL before the parser sees them.

**Estimated effort:** 2–3 sessions (12–18 hours)

### Task 1.1: Mixed UNION / UNION ALL (G1)

**Current behaviour:** A query like `SELECT ... UNION SELECT ... UNION ALL
SELECT ...` is rejected because the parser expects all set operations at the
top level to be the same type.

**Proposed fix:** Add a new auto-rewrite pass (`rewrite_mixed_union`) that
runs after the existing passes. The rewrite detects mixed top-level UNION /
UNION ALL and normalises them:

1. Parse the query with `raw_parser()`.
2. Walk the `SelectStmt` tree to identify the set operation types.
3. If all are the same type, return unchanged.
4. If mixed:
   - UNION (dedup) arms are grouped into a inner `SELECT DISTINCT * FROM
     (arm1 UNION ALL arm2 ...) __pgt_dedup`.
   - UNION ALL arms remain as-is.
   - The final query is `grouped_dedup UNION ALL arm3 UNION ALL arm4 ...`.

This ensures the DVM engine only sees uniform UNION ALL at the top level,
which it already handles. The DISTINCT wrapper provides the dedup semantics.

**Files changed:**
- `src/dvm/parser.rs` — new `rewrite_mixed_union()` function
- `src/dvm/mod.rs` — export and wire into the rewrite pipeline
- `src/api.rs` — call `rewrite_mixed_union()` after existing passes

**Tests:**
- Unit tests in `parser.rs`: 3–4 cases (2 UNION + 1 UNION ALL, 3-way mix,
  nested mix, unchanged when uniform)
- E2E test: create stream table with mixed UNION / UNION ALL, verify delta
  correctness after INSERT on each source

### Task 1.2: Multiple PARTITION BY Resolution (G2)

**Current behaviour:** A defining query with window functions using different
`PARTITION BY` clauses is rejected because the partition recomputation
strategy requires a single partition key.

**Proposed fix:** Add a new auto-rewrite pass (`rewrite_multi_partition`)
that decomposes the query into nested subqueries:

```sql
-- Before rewrite (rejected):
SELECT id, name,
       ROW_NUMBER() OVER (PARTITION BY category ORDER BY price) AS cat_rank,
       SUM(qty) OVER (PARTITION BY region) AS region_total
FROM products

-- After rewrite (supported):
SELECT __inner.id, __inner.name, __inner.cat_rank,
       SUM(__inner.qty) OVER (PARTITION BY __inner.region) AS region_total
FROM (
    SELECT id, name, category, region, qty, price,
           ROW_NUMBER() OVER (PARTITION BY category ORDER BY price) AS cat_rank
    FROM products
) __inner
```

The strategy:
1. Parse the query and collect all window functions.
2. Group by distinct PARTITION BY key.
3. If only one group, return unchanged.
4. Pick the "outermost" partition group (the one referenced by the fewest
   downstream expressions) as the outer window.
5. Push all other window functions into subqueries, layered inside-out.
6. The outermost SELECT applies the final window function on the subquery
   result.

Each layer has a single PARTITION BY, which the DVM engine handles.

**Complexity:** Medium — the rewrite is conceptually straightforward but
extracting and re-assembling window clauses from the raw parse tree requires
careful handling of aliases, ORDER BY, and frame specifications.

**Files changed:**
- `src/dvm/parser.rs` — new `rewrite_multi_partition()` function
- `src/dvm/mod.rs` — export
- `src/api.rs` — wire into rewrite pipeline (after GROUPING SETS, before
  scalar subquery rewrite)

**Tests:**
- Unit tests: 4–5 cases (2 partitions, 3 partitions, mixed with/without
  ORDER BY, ROWS frame, named WINDOW)
- E2E test: create stream table with 2 different PARTITION BY, INSERT data,
  verify both window results update correctly

### Task 1.3: Window Functions Nested in Expressions (G12)

**Current behaviour:** A defining query like `SELECT ABS(ROW_NUMBER() OVER
(...))` is rejected because `expr_contains_window_function()` detects the
nested window function and errors.

**Proposed fix:** Add a new auto-rewrite pass (`rewrite_nested_window_exprs`)
that lifts window functions out of expressions into separate columns, then
wraps the result in an outer SELECT that applies the expression:

```sql
-- Before rewrite (rejected):
SELECT id, ABS(ROW_NUMBER() OVER (ORDER BY score) - 5) AS adjusted_rank
FROM players

-- After rewrite (supported):
SELECT id, ABS(__pgt_wf_1 - 5) AS adjusted_rank
FROM (
    SELECT id, score,
           ROW_NUMBER() OVER (ORDER BY score) AS __pgt_wf_1
    FROM players
) __pgt_wf_inner
```

**Strategy:**
1. Walk the target list looking for expressions containing window functions
   (recursive `expr_contains_window_function()` already exists).
2. For each such expression, extract the window function call into a
   synthetic column in an inner subquery.
3. Replace the window function reference in the outer expression with the
   synthetic column name.
4. Wrap in `SELECT outer_exprs FROM (inner_subquery) __pgt_wf_inner`.

**Files changed:**
- `src/dvm/parser.rs` — new `rewrite_nested_window_exprs()` function
- `src/dvm/mod.rs` — export
- `src/api.rs` — wire into rewrite pipeline (before DISTINCT ON, since
  DISTINCT ON itself may use window functions)

**Tests:**
- Unit tests: 3–4 cases (ABS(ROW_NUMBER()), COALESCE(LAG(), 0), nested
  arithmetic, multiple nested windows in one query)
- E2E test: create stream table with window-in-expression, verify delta

---

## 5. Implementation Plan — Phase 2: New DVM Operators

**Goal:** Implement 3 genuinely new DVM operators for constructs that cannot
be resolved by auto-rewriting to existing operators.

**Estimated effort:** 3–4 sessions (18–24 hours)

### Task 2.1: ALL (Subquery) Operator (G3)

**Current behaviour:** `WHERE col > ALL (SELECT x FROM t)` is rejected with
a suggestion to use `NOT EXISTS`.

**Proposed fix:** Implement via rewrite to NOT EXISTS at the OpTree level
(not SQL rewrite). `ALL (SELECT ...)` is semantically: "the predicate holds
for every row returned by the subquery." This is the negation of `EXISTS
(SELECT ... WHERE NOT predicate)`.

**Implementation:**
1. In `extract_where_sublinks()` (parser.rs), detect `ALL_SUBLINK` nodes.
2. Negate the comparison operator (e.g., `>` → `<=`).
3. Construct an `AntiJoin` OpTree node with the negated condition — this
   emits "rows from the left that have NO match in the right where the
   negated condition holds," which is equivalent to ALL.
4. Alternatively, rewrite to `NOT EXISTS (SELECT 1 FROM subquery WHERE NOT
   (col op subquery_col))` at the SQL level before parsing.

**Recommended approach:** SQL-level rewrite (new `rewrite_all_sublink()`
pass) is simpler and leverages the existing AntiJoin operator without new
OpTree variants or diff operators.

**Files changed:**
- `src/dvm/parser.rs` — new `rewrite_all_sublink()` or extend
  `extract_where_sublinks()` to handle `ALL_SUBLINK`
- `src/dvm/mod.rs` — export if SQL-level rewrite
- `src/api.rs` — wire into pipeline if SQL-level rewrite

**Tests:**
- Unit tests: 3 cases (ALL with `>`, `=`, `<>` operators)
- E2E test: stream table with `WHERE price > ALL (SELECT ...)`, verify
  delta correctness for INSERT/DELETE in both outer and inner tables

### Task 2.2: Deeply Nested SubLinks in OR (G4)

**Current behaviour:** The `rewrite_sublinks_in_or()` pass handles
`WHERE a OR EXISTS (...)` at the top level. Deeply nested patterns like
`WHERE x AND (y OR EXISTS (...))` survive the rewrite and are rejected.

**Proposed fix:** Extend `rewrite_sublinks_in_or()` to recursively walk
the boolean expression tree:

1. **Base case:** If the node is not a BoolExpr, return unchanged.
2. **AND node:** Recurse into each argument. If any argument becomes a
   UNION after rewriting, the AND condition must be applied as a WHERE
   filter on that UNION branch.
3. **OR node (current):** Already decomposes into UNION branches. No change.
4. **OR node (nested):** An OR inside an AND is handled by step 2 — the
   AND's other arms become WHERE filters on the decomposed UNION.

**Example:**
```sql
-- Before (rejected):
SELECT * FROM t WHERE x = 1 AND (y = 2 OR EXISTS (SELECT 1 FROM s WHERE s.id = t.id))

-- After rewrite (supported):
SELECT * FROM t WHERE x = 1 AND y = 2
UNION ALL
SELECT * FROM t WHERE x = 1 AND EXISTS (SELECT 1 FROM s WHERE s.id = t.id)
```

The key insight: the AND conjuncts that are NOT part of the OR are
duplicated into each UNION branch. The OR arms become separate branches.

**Complexity:** Medium-High — the recursive tree rewriting is conceptually
clean but requires careful handling of NOT, double negation, and mixed
AND/OR/NOT nesting.

**Files changed:**
- `src/dvm/parser.rs` — extend `rewrite_sublinks_in_or()` or split into
  `rewrite_sublinks_in_bool_tree()`
- `src/dvm/mod.rs` — export
- `src/api.rs` — replace existing call

**Tests:**
- Unit tests: 5–6 cases (nested AND(OR(EXISTS)), double nested,
  NOT(OR(EXISTS)), mixed AND/OR/NOT, already-clean query unchanged)
- E2E test: stream table with nested OR + EXISTS, verify delta

### Task 2.3: ROWS FROM() with Multiple Functions (G5)

**Current behaviour:** `SELECT * FROM ROWS FROM(unnest(a), unnest(b))` is
rejected.

**Proposed fix:** Rewrite to `LATERAL` with explicit zip logic. PostgreSQL's
`ROWS FROM()` zips multiple SRF outputs column-by-column, padding shorter
results with NULL. This can be expressed as:

```sql
-- Before (rejected):
SELECT * FROM ROWS FROM(unnest(ARRAY[1,2,3]), unnest(ARRAY['a','b']))

-- After rewrite (supported):
SELECT f1.unnest AS col1, f2.unnest AS col2
FROM generate_series(1, GREATEST(
       array_length(ARRAY[1,2,3], 1),
       array_length(ARRAY['a','b'], 1)
     )) __pgt_idx
LEFT JOIN LATERAL unnest(ARRAY[1,2,3]) WITH ORDINALITY AS f1(unnest, ord)
  ON f1.ord = __pgt_idx
LEFT JOIN LATERAL unnest(ARRAY['a','b']) WITH ORDINALITY AS f2(unnest, ord)
  ON f2.ord = __pgt_idx
```

This is correct but complex to auto-generate for arbitrary SRFs. A simpler
approach for the common case (all SRFs are `unnest`):

```sql
-- Simplified for unnest-of-arrays:
SELECT u1, u2
FROM unnest(ARRAY[1,2,3], ARRAY['a','b']) AS t(u1, u2)
```

PostgreSQL's multi-argument `unnest()` already does zip semantics.

**Recommendation:** Implement as SQL-level rewrite. Detect `ROWS FROM()`
nodes in the raw parse tree, extract the function calls, and rewrite to
multi-argument `unnest()` (when all are `unnest`) or to the explicit LATERAL
with `generate_series` (general case).

**Files changed:**
- `src/dvm/parser.rs` — new `rewrite_rows_from()` function
- `src/dvm/mod.rs` — export
- `src/api.rs` — wire into pipeline

**Tests:**
- Unit tests: 3 cases (dual unnest, triple unnest, mixed SRFs)
- E2E test: stream table over `ROWS FROM()`, verify delta

### Task 2.4: LATERAL with RIGHT/FULL JOIN (G6)

**Current behaviour:** `RIGHT JOIN LATERAL` and `FULL JOIN LATERAL` are
rejected.

**Proposed fix:** This is a genuine PostgreSQL semantic constraint, not just
a pg_trickle limitation. PostgreSQL itself rejects `RIGHT JOIN LATERAL` and
`FULL JOIN LATERAL` in most contexts because the LATERAL reference creates a
dependency from the right side to the left side, which conflicts with the
semantics of RIGHT/FULL JOIN where the right side must be evaluated
independently.

**Recommendation:** Keep the rejection. This is a PostgreSQL-level constraint,
not a pg_trickle limitation. Document it clearly in the error message:

```
LATERAL subqueries can only be used with INNER JOIN or LEFT JOIN. RIGHT JOIN
LATERAL and FULL JOIN LATERAL are not supported by PostgreSQL because the
lateral reference creates a dependency from the right side to the left side.
```

**Action:** Update the error message to clarify this is a PostgreSQL
constraint, not a pg_trickle limitation.

---

## 6. Implementation Plan — Phase 3: Trigger-Level Optimisations

**Goal:** Reduce the write-path overhead of trigger-based CDC and improve
buffer table management. These changes are in `src/cdc.rs` and the PL/pgSQL
trigger functions — they do not affect the DVM engine or SQL coverage.

**Estimated effort:** 4–6 sessions (24–36 hours)

### Task 3.1: Column-Level Change Detection for UPDATE

**Current behaviour:** The UPDATE trigger writes all NEW and OLD column
values to the buffer table, regardless of which columns changed.

**Proposed fix:** Modify the generated PL/pgSQL trigger function to compare
NEW and OLD values and only write columns that actually changed. Unchanged
columns are stored as NULL in the buffer (with a bitmask column indicating
which columns are populated).

**Implementation:**

1. Add a `changed_cols` BIGINT bitmask column to the change buffer table
   (bit N = 1 means column N changed).
2. Generate the UPDATE trigger body with per-column `IS DISTINCT FROM`
   comparisons:

```sql
IF TG_OP = 'UPDATE' THEN
    INSERT INTO pgtrickle_changes.changes_<oid>
        (lsn, action, pk_hash, changed_cols,
         new_col1, old_col1, new_col2, old_col2, ...)
    VALUES (
        pg_current_wal_insert_lsn(), 'U', <pk_hash>,
        (CASE WHEN NEW."col1" IS DISTINCT FROM OLD."col1" THEN 1 ELSE 0 END) |
        (CASE WHEN NEW."col2" IS DISTINCT FROM OLD."col2" THEN 2 ELSE 0 END) | ...
        ,
        CASE WHEN NEW."col1" IS DISTINCT FROM OLD."col1" THEN NEW."col1" END,
        CASE WHEN NEW."col1" IS DISTINCT FROM OLD."col1" THEN OLD."col1" END,
        CASE WHEN NEW."col2" IS DISTINCT FROM OLD."col2" THEN NEW."col2" END,
        CASE WHEN NEW."col2" IS DISTINCT FROM OLD."col2" THEN OLD."col2" END,
        ...
    );
    RETURN NEW;
END IF;
```

3. The scan delta operator reads `changed_cols` and only references populated
   columns. For columns not in the bitmask, it uses the current table value
   (via a JOIN to the source table by PK).

**Estimated overhead reduction:** 30–60% for UPDATE-heavy workloads on tables
with 10+ columns where typical updates touch 1–2 columns. The per-column
`IS DISTINCT FROM` adds ~0.5 μs per column but saves the I/O cost of writing
and indexing the unchanged values.

**Caveat:** This optimisation only applies to UPDATE; INSERT and DELETE still
write all columns (INSERT has no OLD, DELETE has no NEW — both need all
values for delta computation).

**Files changed:**
- `src/cdc.rs` — modify `create_change_buffer_table()` (add `changed_cols`
  column) and `create_change_trigger()` (conditional column writes)
- `src/dvm/operators/scan.rs` — modify `diff_scan_change_buffer()` to use
  `changed_cols` bitmask when available (backward-compatible: NULL bitmask
  means all columns populated)

**Migration:** Existing buffer tables do not have the `changed_cols` column.
Add it via `ALTER TABLE ... ADD COLUMN IF NOT EXISTS changed_cols BIGINT`
during the next DDL event trigger rebuild or via an explicit upgrade function.

**Tests:**
- Unit tests: scan delta with partial column changes
- E2E test: UPDATE 1 column in a 10-column table, verify correct delta and
  reduced buffer row size

### Task 3.2: Incremental TRUNCATE Handling

**Current behaviour:** TRUNCATE on a source table writes a 'T' marker →
the refresh engine falls back to FULL refresh (TRUNCATE stream table +
repopulate from defining query).

**Proposed fix:** For stream tables with simple defining queries (Scan →
Filter → Project, no JOINs or aggregates), compute a "negation delta"
instead of a full refresh:

1. Detect the 'T' marker in the change buffer.
2. Instead of FULL refresh, compute:
   ```sql
   -- Negation delta: delete all current rows
   DELETE FROM stream_table WHERE __pgt_row_id IN (
       SELECT __pgt_row_id FROM stream_table
   )
   ```
3. Then scan the (now-empty) source table for any rows inserted after
   TRUNCATE (in the same transaction or later) and insert those.

**Benefit:** For stream tables that are much smaller than a full repopulation,
the negation delta is faster than TRUNCATE + INSERT. For aggregate stream
tables, the negation delta can be even more targeted (delete only affected
groups).

**Caveat:** For stream tables with JOINs, TRUNCATE of one source doesn't
mean the stream table should be empty — it depends on the join type. FULL
refresh remains the correct fallback for multi-source stream tables.

**Files changed:**
- `src/refresh.rs` — add `execute_truncate_delta()` function, called from
  the TRUNCATE detection block instead of `execute_full_refresh()` when
  the stream table has a simple single-source defining query

**Tests:**
- E2E test: TRUNCATE source table, verify stream table is correctly emptied
  via differential path (not full refresh), then INSERT new rows and verify
  delta propagation

### Task 3.3: Buffer Table Partitioning by LSN Range

**Current behaviour:** Change buffer tables are unpartitioned heap tables.
After each refresh cycle, consumed rows are deleted via
`DELETE FROM changes_<oid> WHERE lsn <= frontier`. VACUUM must reclaim dead
tuples.

**Proposed fix:** Partition buffer tables by LSN range. Each partition covers
one refresh cycle's worth of changes:

1. At `create_stream_table()` time, create the buffer table as a partitioned
   table: `PARTITION BY RANGE (lsn)`.
2. Before each refresh, create a new partition for the current LSN range.
3. After consuming changes for a refresh, detach the old partition and
   DROP it (instant, no VACUUM needed).

**Benefit:** Eliminates VACUUM overhead on buffer tables entirely. DROP is
O(1) vs DELETE + VACUUM which is O(n). Especially impactful for high-write
sources.

**Caveat:** Partition management adds DDL overhead per refresh cycle.
PostgreSQL's native partitioning has diminishing returns for very short
refresh cycles (<10s) where partition create/detach overhead exceeds the
vacuum savings.

**Recommendation:** Make this optional, controlled by a GUC
(`pg_trickle.buffer_partitioning = 'auto' | 'on' | 'off'`). Default 'auto'
enables partitioning for sources with refresh cycles ≥ 30s.

**Files changed:**
- `src/cdc.rs` — modify `create_change_buffer_table()` to support
  partitioned mode
- `src/refresh.rs` — partition management: create new partition before
  refresh, detach + drop old partition after consuming
- `src/config.rs` — new `pg_trickle.buffer_partitioning` GUC

**Tests:**
- E2E test: create stream table with partitioned buffer, run multiple
  refresh cycles, verify no VACUUM needed and dead tuples = 0

### Task 3.4: Skip-Unchanged-Column Scanning in Delta

**Current behaviour:** The scan delta reads all columns from the change
buffer for every change row, even when the defining query only references
a subset of columns.

**Proposed fix:** The `Scan` OpTree node already knows which columns are
referenced by the defining query (the `columns` field). The delta SQL should
only SELECT the referenced columns from the buffer, reducing I/O:

```sql
-- Current: reads all buffer columns
SELECT pk_hash, action, new_name, old_name, new_price, old_price, new_category, old_category
FROM changes_<oid>
WHERE lsn > $prev AND lsn <= $new

-- Proposed: only reads columns used by the defining query
-- (defining query: SELECT name, price FROM products WHERE ...)
SELECT pk_hash, action, new_name, old_name, new_price, old_price
FROM changes_<oid>
WHERE lsn > $prev AND lsn <= $new
```

PostgreSQL's heap access still reads full tuples, so the I/O savings are
marginal for narrow tables. However, for wide tables (50+ columns) where
the stream table only uses 5–10 columns, this can significantly reduce the
data transferred to the DVM engine.

**Files changed:**
- `src/dvm/operators/scan.rs` — modify `diff_scan_change_buffer()` to
  project only referenced columns

**Tests:**
- Unit tests: delta SQL generation with subset of columns

### Task 3.5: Online Trigger Rebuild Without Full Reinit

**Current behaviour:** When a source table's schema changes (ALTER TABLE ADD
COLUMN), the DDL event trigger sets `needs_reinit = true`, which forces a
full refresh on the next scheduler cycle. The trigger function is rebuilt
and the buffer table schema is updated, but all existing buffer data is
discarded.

**Proposed fix:** For additive schema changes (ADD COLUMN), avoid the full
reinit:

1. `ALTER TABLE changes_<oid> ADD COLUMN IF NOT EXISTS "new_colN" TYPE,
   "old_colN" TYPE` — add new columns to the buffer.
2. `CREATE OR REPLACE FUNCTION ...` — rebuild the trigger function with
   the new column list.
3. Existing buffer rows have NULL for the new columns (correct — the
   new column didn't exist when those changes were captured).
4. The scan delta handles NULL as "column not present in this change"
   (compatible with the column-level change detection from Task 3.1).
5. **No** full refresh needed — the next differential refresh processes
   existing buffer data correctly, and new changes include the new column.

**Caveat:** DROP COLUMN and RENAME COLUMN still require full reinit because
the buffer column names are tied to the old schema.

**Files changed:**
- `src/hooks.rs` — modify DDL event handler to distinguish ADD COLUMN from
  other ALTER TABLE subcommands; only set `needs_reinit = true` for
  destructive schema changes

**Tests:**
- E2E test: ADD COLUMN on source table without full reinit, verify next
  differential refresh works correctly with mixed old/new buffer rows

---

## 7. Implementation Plan — Phase 4: Remaining Aggregate Coverage

**Goal:** Implement the remaining 28 aggregate functions using the proven
group-rescan strategy, bringing the total from 25 to 53.

**Estimated effort:** 2–3 sessions (12–18 hours)

### Task 4.1: Regression Aggregates (G7) — 11 Functions

Implement `CORR`, `COVAR_POP`, `COVAR_SAMP`, `REGR_AVGX`, `REGR_AVGY`,
`REGR_COUNT`, `REGR_INTERCEPT`, `REGR_R2`, `REGR_SLOPE`, `REGR_SXX`,
`REGR_SXY`, `REGR_SYY`.

These are all two-argument aggregate functions that take (Y, X) pairs.
The group-rescan strategy applies directly:

1. Detect affected groups from the delta (groups where any row was
   inserted, updated, or deleted).
2. For each affected group, re-aggregate from source:
   `SELECT group_key, CORR(y, x) AS corr_val FROM source GROUP BY group_key`
3. Apply the result as an UPDATE to the stream table.

**Files changed:**
- `src/dvm/parser.rs` — add `AggFunc::Corr`, `AggFunc::CovarPop`,
  `AggFunc::CovarSamp`, and 8 `AggFunc::Regr*` variants.
  Update `parse_aggregate()` to recognise these function names.
- `src/dvm/operators/aggregate.rs` — add cases to `group_rescan_agg_sql()`
  for each new variant. The group-rescan pattern is identical to existing
  aggregates (BOOL_AND, STRING_AGG, etc.) — emit the aggregate function
  call over the source table grouped by the same GROUP BY keys.

**Tests:**
- Unit tests: 11 tests (one per aggregate), verifying delta SQL generation
- E2E test: stream table with `CORR(y, x)`, INSERT/DELETE, verify delta

### Task 4.2: Hypothetical-Set Aggregates (G8) — 4 Functions

Implement `RANK()`, `DENSE_RANK()`, `PERCENT_RANK()`, `CUME_DIST()` as
**aggregate functions** (not window functions — these are already supported
as window functions).

Hypothetical-set aggregates use `WITHIN GROUP (ORDER BY ...)` syntax:
```sql
SELECT RANK(42) WITHIN GROUP (ORDER BY score) FROM students
```

The group-rescan strategy works here too: for affected groups, re-aggregate
from source. The aggregate call syntax includes the hypothetical value and
the ORDER BY clause.

**Files changed:**
- `src/dvm/parser.rs` — add `AggFunc::HypRank`, `AggFunc::HypDenseRank`,
  `AggFunc::HypPercentRank`, `AggFunc::HypCumeDist` variants.  Extend
  the ordered-set aggregate parsing (already exists for MODE/PERCENTILE)
  to recognise these.
- `src/dvm/operators/aggregate.rs` — group-rescan with hypothetical value

**Tests:**
- Unit tests: 4 tests (one per aggregate)
- E2E test: stream table with `RANK(x) WITHIN GROUP (ORDER BY ...)`,
  INSERT/DELETE, verify delta

### Task 4.3: XMLAGG (G9)

Implement `XMLAGG(expr ORDER BY ...)`. Group-rescan strategy. Very niche
but trivial given the established pattern.

**Files changed:**
- `src/dvm/parser.rs` — add `AggFunc::XmlAgg` variant
- `src/dvm/operators/aggregate.rs` — group-rescan

**Tests:**
- Unit test: delta SQL generation
- E2E test: stream table with XMLAGG, verify delta

---

## 8. Implementation Plan — Phase 5: IMMEDIATE Mode Parity

**Goal:** Extend IMMEDIATE mode to support constructs currently rejected
by `validate_immediate_mode_support()`, closing the gap with DIFFERENTIAL
mode.

**Estimated effort:** 2–3 sessions (12–18 hours)

### Task 5.1: Recursive CTEs in IMMEDIATE Mode (G10)

**Current status:** `RecursiveCte` is the only OpTree node rejected by
`check_immediate_support()`.

**Root cause:** Recursive CTEs use semi-naive evaluation with fixpoint
iteration. In IMMEDIATE mode, the delta source is a transition table (temp
table), not a change buffer. The concern is that fixpoint iteration within
a trigger function may:
1. Exceed PostgreSQL's `max_stack_depth` for deeply recursive CTEs.
2. Interact incorrectly with the trigger's transaction state (SPIs, snapshots).

**Proposed fix:**

1. **Validate** that the existing DVM semi-naive evaluation works correctly
   with `DeltaSource::TransitionTable` by running the recursive CTE E2E
   tests from DIFFERENTIAL mode against IMMEDIATE mode.
2. **If correct:** Remove the `RecursiveCte` rejection from
   `check_immediate_support()`. Add a warning about potential stack depth
   issues.
3. **If incorrect:** Implement a fallback to FULL recomputation for
   recursive CTEs in IMMEDIATE mode (truncate + repopulate the stream
   table on each trigger firing, similar to TRUNCATE handling).

**Files changed:**
- `src/dvm/parser.rs` — remove `RecursiveCte` arm from
  `check_immediate_support()`
- `src/ivm.rs` — add stack depth guard before recursive CTE delta execution

**Tests:**
- E2E test: create IMMEDIATE stream table with `WITH RECURSIVE`, INSERT
  into base table, verify stream table is updated correctly within the
  same transaction

### Task 5.2: TopK in IMMEDIATE Mode (G11)

**Current status:** TopK (`ORDER BY + LIMIT`) is rejected in IMMEDIATE mode
because scoped recomputation is a deferred/full pattern.

**Root cause:** TopK uses a different delta strategy than standard
differential: instead of delta algebra, it maintains a bounded result set
via scoped recomputation (delete rows that fall outside the top K, insert
rows that enter it). This is orchestrated by the refresh engine, not the
trigger function.

**Proposed fix:** Implement statement-level TopK maintenance in the IVM
trigger:

1. After applying the delta from transition tables, compute the new top K:
   ```sql
   SELECT * FROM (defining_query) __pgt_topk LIMIT K
   ```
2. Diff the new top K against the current stream table contents.
3. Apply DELETE + INSERT for rows that entered/exited the top K.

This is essentially a micro-full-refresh scoped to the K rows. For small K
(e.g., top 10, top 100), this is fast enough for inline trigger execution.

**Caveat:** For large K (>10,000), the inline recomputation may add
unacceptable latency to the triggering DML. Add a GUC guard:
`pg_trickle.ivm_topk_max_limit` (default 1000). TopK queries with
LIMIT > this threshold are rejected in IMMEDIATE mode.

**Files changed:**
- `src/api.rs` — remove TopK + IMMEDIATE rejection (replace with limit
  threshold check)
- `src/ivm.rs` — add TopK-specific delta computation in
  `pgt_ivm_apply_delta()`: after standard delta, if TopK detected,
  recompute bounded result and apply diff
- `src/config.rs` — new `pg_trickle.ivm_topk_max_limit` GUC

**Tests:**
- E2E test: IMMEDIATE stream table with `ORDER BY score DESC LIMIT 10`,
  INSERT rows, verify top-10 list updates within same transaction
- E2E test: LIMIT exceeding threshold → rejection at creation time

---

## 9. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|-----------|
| **Auto-rewrite correctness** | High | Each rewrite must preserve query semantics exactly. Extensive unit + E2E tests with known-answer comparisons between original and rewritten queries. |
| **Mixed UNION rewrite performance** | Medium | The DISTINCT wrapper adds a sort/hash. For large result sets, this may be slower than the original query. Document the trade-off. |
| **Column-level change detection overhead** | Medium | Per-column `IS DISTINCT FROM` in the trigger adds ~0.5 μs × num_columns. For narrow tables this overhead exceeds the savings. Only enable for tables with 10+ columns. |
| **Buffer partitioning DDL overhead** | Medium | CREATE/DETACH PARTITION per refresh cycle. For very short cycles (<10s), this overhead may exceed vacuum savings. Guard with 30s minimum. |
| **Recursive CTEs in IMMEDIATE mode** | High | Stack depth exhaustion. Add explicit `max_stack_depth` check before fixpoint iteration. Fall back to full recomputation if depth exceeded. |
| **TopK inline recomputation latency** | Medium | Large K values add unacceptable latency. Guard with `ivm_topk_max_limit` GUC. |
| **Backward compatibility of buffer table schema** | Low | New `changed_cols` column must be optional (NULL for old rows). Delta scan must handle both formats. |

---

## 10. Success Criteria

After all phases are complete:

- [ ] Zero SQL constructs silently produce wrong results (maintained — no
      regressions).
- [ ] Mixed UNION / UNION ALL creates and refreshes correctly in
      DIFFERENTIAL mode.
- [ ] Multiple PARTITION BY windows work via auto-rewrite.
- [ ] Window functions nested in expressions work via auto-rewrite.
- [ ] ALL (subquery) works via rewrite to NOT EXISTS.
- [ ] Deeply nested SubLinks in OR work via extended rewrite.
- [ ] ROWS FROM() with multiple functions works via rewrite.
- [ ] 36+ aggregate functions in DIFFERENTIAL mode (up from 25).
- [ ] Recursive CTEs work in IMMEDIATE mode (with stack guard).
- [ ] TopK works in IMMEDIATE mode (with limit guard).
- [ ] Column-level change detection reduces UPDATE buffer size by 30–60%
      for wide tables.
- [ ] Buffer table partitioning eliminates vacuum overhead for high-write
      sources.
- [ ] Online ADD COLUMN does not force full reinit.
- [ ] 900+ unit tests.
- [ ] Documentation fully consistent (FAQ, SQL_REFERENCE, DVM_OPERATORS).

---

## 11. Prioritised Execution Order

Phases are ordered by **user-visible impact** relative to implementation
effort. Each phase is independently shippable.

```
Phase 1 — Auto-Rewrite Completions          (2–3 sessions, 12–18 hours)
  ├─ Task 1.1: Mixed UNION / UNION ALL      → Unblocks common analytics patterns
  ├─ Task 1.2: Multiple PARTITION BY        → Unblocks multi-window queries
  └─ Task 1.3: Nested window expressions    → Unblocks computed window results

Phase 2 — New DVM Operators                  (3–4 sessions, 18–24 hours)
  ├─ Task 2.1: ALL (subquery)              → Completes subquery coverage
  ├─ Task 2.2: Deeply nested SubLinks/OR   → Removes last OR restriction
  └─ Task 2.3: ROWS FROM() multi-function  → Niche, low priority

Phase 3 — Trigger-Level Optimisations        (4–6 sessions, 24–36 hours)
  ├─ Task 3.1: Column-level change detect   → Biggest win for UPDATE-heavy
  ├─ Task 3.2: Incremental TRUNCATE         → Improves ETL patterns
  ├─ Task 3.3: Buffer table partitioning    → Eliminates vacuum overhead
  ├─ Task 3.4: Skip-unchanged-col scanning  → Reduces delta read I/O
  └─ Task 3.5: Online ADD COLUMN            → Avoids unnecessary full reinit

Phase 4 — Remaining Aggregates               (2–3 sessions, 12–18 hours)
  ├─ Task 4.1: Regression aggregates (11)   → Statistical analysis coverage
  ├─ Task 4.2: Hypothetical-set aggs (4)    → Completes ordered-set coverage
  └─ Task 4.3: XMLAGG                       → Niche, trivial

Phase 5 — IMMEDIATE Mode Parity             (2–3 sessions, 12–18 hours)
  ├─ Task 5.1: Recursive CTEs              → Closes last IMMEDIATE gap
  └─ Task 5.2: TopK                         → Bounded top-N in transactions

Total estimated effort: 13–19 sessions (~80–110 hours)
```

### Recommended First Session

**Phase 1, Tasks 1.1 + 1.3** — Mixed UNION / UNION ALL + nested window
expressions. These are the most commonly requested features and follow
the well-established auto-rewrite pattern. Both can be completed in a single
session and immediately expand coverage for analytics queries.

### What NOT to Prioritise

| Item | Why Defer |
|------|-----------|
| **LATERAL with RIGHT/FULL JOIN (G6)** | PostgreSQL itself rejects this. Not a pg_trickle limitation. |
| **ROWS FROM() multi-function (G5)** | Very niche. Almost never used in analytics. |
| **XMLAGG (G9)** | Very niche. Trivial to add but low demand. |

---

## 12. ADR Reference

This plan would result in the following new ADRs:

### ADR-009: Auto-Rewrite Pipeline Extensions

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Category** | DVM Engine |
| **Date** | 2026-03-03 |

**Decision:** Extend the 5-pass auto-rewrite pipeline to 8 passes (mixed
UNION normalisation, multi-PARTITION BY decomposition, nested window
expression lifting) rather than implementing new DVM operators. This
maximises SQL coverage with minimal engine complexity.

**Rationale:** Each rewrite transforms an unsupported pattern into one that
the existing DVM operators already handle correctly. This avoids new OpTree
variants, new diff operators, and the associated test surface. The rewrite
correctness is verifiable by checking that the rewritten query produces
identical results to the original on static data.

### ADR-010: Column-Level Change Detection in Trigger CDC

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Category** | CDC Engine |
| **Date** | 2026-03-03 |

**Decision:** Add optional column-level change detection to the UPDATE CDC
trigger, storing only modified columns in the change buffer with a bitmask.
This reduces buffer write volume by 30–60% for UPDATE-heavy workloads on
wide tables.

**Trade-off:** Adds ~0.5 μs per column of comparison overhead in the trigger.
Net positive for tables with 10+ columns; net negative for very narrow tables.
Controlled by automatic threshold (tables with <10 columns use the current
full-row capture).
