# Plan: Expanding SQL Coverage in Trigger-Based CDC Mode (Part 2)

**Date:** 2026-03-03  
**Last updated:** 2026-03-08  
**Status:** ALL phases complete. Stage 3 (Tasks 1.1–2.4) complete. Stage 4 (EC-16, Tasks 3.1–3.5) complete. Stage 5 (Tasks 4.1–4.3) complete. Task 2.3 (ROWS FROM) implemented.  
**Branch:** `edge-cases-and-transactional-ivm`  
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

**Current state (ground truth as of 2026-03-04):**

- 1032 unit tests, 22+ E2E test suites passing
- 25 aggregate functions in DIFFERENTIAL mode
- 21 OpTree variants, 20 diff operators
- 8 auto-rewrite passes (view inlining, nested window expressions, DISTINCT ON,
  GROUPING SETS, scalar subquery in WHERE, SubLinks in OR, SubLinks in AND+OR,
  SubLinks in deeply nested AND chains)
- ALL (subquery) → NULL-safe AntiJoin via `parse_all_sublink()` (G3 closed)
- NATURAL JOIN fully resolved at parse time
- TRUNCATE on source table: statement-level trigger → automatic FULL refresh fallback
- TRUNCATE on stream table: blocked by BEFORE TRUNCATE guard trigger (EC-25)
- Direct DML on stream table: blocked by BEFORE I/U/D guard trigger (EC-26)
- Keyless tables (no PK): fully supported via net-counting delta (EC-06)
- Mixed UNION / UNION ALL handled natively (no rewrite needed)
- Multiple PARTITION BY handled natively via full recomputation (no rewrite needed)
- Nested window expressions lifted to inner subquery via `rewrite_nested_window_exprs()` (**G12 closed**)

**After this plan (target):**

- 900+ unit tests
- 36+ aggregate functions in DIFFERENTIAL mode (11 regression + hypothetical)
- ✅ Multiple PARTITION BY resolved via query-level rewrite (done natively)
- ✅ Mixed UNION / UNION ALL handled natively
- ✅ Nested window expressions: `rewrite_nested_window_exprs()` subquery lift
- ✅ ALL (subquery): `parse_all_sublink()` NULL-safe AntiJoin (G3 closed)
- ✅ Deeply nested SubLinks in OR: `flatten_and_conjuncts()` handles AND(AND(OR(EXISTS))) (G4 closed)
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
| **G1** | Mixed UNION / UNION ALL | ✅ **Works natively** | — | `collect_union_children` + `Distinct`/`UnionAll` OpTree handle mixed arms | 1 |
| **G2** | Multiple PARTITION BY in one query | ✅ **Works natively** | — | Full recomputation approach; no SQL rewrite needed | 1 |
| **G3** | ALL (subquery) | ✅ **DONE** — NULL-safe AntiJoin | `parse_all_sublink()` builds `(col IS NULL OR NOT (x op col))` condition; full AntiJoin diff pipeline already wired | EC-32 closed | 1 |
| **G4** | SubLinks inside deeply nested OR | ✅ **DONE** — handles `AND(AND(OR(EXISTS)))` | `flatten_and_conjuncts()` helper + updated `and_contains_or_with_sublink()` + `rewrite_and_with_or_sublinks()` flattens AND tree recursively | — | 2 |
| **G5** | ROWS FROM() with multiple functions | ✅ **DONE** — `rewrite_rows_from()` auto-rewrite | All-unnest optimisation merges to multi-arg `unnest()`; general case uses ordinal LEFT JOIN LATERAL chain | 2 |
| **G6** | LATERAL with RIGHT/FULL JOIN | Rejected (PostgreSQL constraint) | PostgreSQL itself rejects RIGHT/FULL JOIN LATERAL | Error message updated to explain PostgreSQL-level constraint (**message improved**) | — |
| **G7** | Regression aggregates (11 functions) | ✅ **DONE** — group-rescan | Already fully wired in prior session (`AggFunc::Corr`, `CovarPop`, `RegrAvgx` etc. + match arms + `is_group_rescan`) | — | 4 |
| **G8** | Hypothetical-set aggregates (4 funcs) | ✅ **DONE** — group-rescan | `HypRank/HypDenseRank/HypPercentRank/HypCumeDist` variants; `is_ordered_set` updated in `agg_to_rescan_sql` | — | 4 |
| **G9** | XMLAGG | ✅ **DONE** — group-rescan | `AggFunc::XmlAgg`; `sql_name()` → `"XMLAGG"`; `is_group_rescan()` true | — | 4 |
| **G10** | Recursive CTEs in IMMEDIATE mode | Rejected | Fixpoint iteration not validated with transition tables | Validate and enable | 5 |
| **G11** | TopK in IMMEDIATE mode | Rejected | Scoped recompute is a deferred/full pattern | Statement-level TopK maintenance | 5 |
| **G12** | Window functions nested in expressions | ✅ **DONE** — auto-rewritten | `rewrite_nested_window_exprs()` lifts nested window funcs to inner subquery | Wired in `api.rs` before DISTINCT ON | 1 |

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

### Task 1.1: Mixed UNION / UNION ALL (G1) — ✅ ALREADY WORKS NATIVELY

**Discovery:** Mixed UNION / UNION ALL queries work natively in the DVM
engine without any SQL rewrite pass. `collect_union_children` in `parser.rs`
preserves the nested `Distinct{UnionAll{...}}` structure: UNION arms are
wrapped in a `Distinct` OpTree node, UNION ALL arms are wrapped in
`UnionAll`. The `diff_distinct` and `diff_union_all` operators both handle
this correctly. No `rewrite_mixed_union()` function is needed or planned.

**Action:** None. Close G1 as already handled.

### Task 1.2: Multiple PARTITION BY Resolution (G2) — ✅ HANDLED NATIVELY

**Discovery:** Multiple PARTITION BY window functions in one query are
handled natively via the full-recomputation path. The `rewrite_multi_partition_windows()`
function exists in `src/dvm/parser.rs` but is **not exported from `mod.rs`
and not called in `api.rs`** — it is dead code. The working approach uses the
existing window function analysis that treats queries with different PARTITION
BY clauses as un-partitioned (full recomputation applies). An api.rs comment
confirms: "Window functions with different PARTITION BY clauses are now
handled by the parser as un-partitioned (full recomputation). No SQL rewrite
needed."

**Action:** None. Close G2 as already handled natively. The dead
`rewrite_multi_partition_windows()` function may be removed in a future
cleanup pass.

### Task 1.3: Window Functions Nested in Expressions (G12) — ✅ DONE

**Status:** **IMPLEMENTED** — `rewrite_nested_window_exprs()` in
`src/dvm/parser.rs`, exported from `src/dvm/mod.rs`, wired in `src/api.rs`
before the DISTINCT ON rewrite. The helper `collect_all_window_func_nodes()`
recursively harvests all FuncCall-with-OVER nodes from an expression tree.
The `deparse_select_window_clause()` helper carries the WINDOW clause
(named window references) into the inner SELECT.

**What was implemented:**
- `rewrite_nested_window_exprs(query)` → lifts nested window funcs to inner subquery
- `collect_all_window_func_nodes(node, result)` → recursive walker
- `deparse_select_window_clause(select)` → deparsers the WINDOW clause
- Bail-out conditions preserved: set operations, GROUP BY, no nested window funcs found

**Before/after:**
```sql
-- Before (rejected):
SELECT id, ABS(ROW_NUMBER() OVER (ORDER BY score) - 5) AS adjusted_rank FROM players;

-- After rewrite (accepted by DVM):
SELECT id, ABS("__pgt_wf_inner"."__pgt_wf_1" - 5) AS "adjusted_rank"
  FROM (SELECT *, ROW_NUMBER() OVER (ORDER BY score) AS "__pgt_wf_1" FROM players) "__pgt_wf_inner";
```

**Note:** EC-03 in PLAN_EDGE_CASES.md originally proposed a CTE-based lift.
That approach is superseded by this subquery lift. The CTE approach produces
a `WithQuery` node in the OpTree requiring the DVM engine to handle CTEs
wrapping window functions — an untested path. The subquery approach produces
a nested `Scan → Window → Project` chain that the existing DVM path already
handles correctly. EC-03 has been updated accordingly.

---

## 5. Implementation Plan — Phase 2: New DVM Operators

**Goal:** Implement 3 genuinely new DVM operators for constructs that cannot
be resolved by auto-rewriting to existing operators.

**Estimated effort:** 3–4 sessions (18–24 hours)

### Task 2.1: ALL (Subquery) Operator (G3) — ✅ DONE

**Status:** **IMPLEMENTED** — `parse_all_sublink()` in `src/dvm/parser.rs` now
uses a NULL-safe anti-join condition. The full pipeline was already wired:
- `parse_sublink_to_wrapper()` routes `ALL_SUBLINK` → `parse_all_sublink()`
- `parse_all_sublink()` builds an `AntiJoin` `SublinkWrapper`
- `diff_anti_join()` in `operators/anti_join.rs` generates the delta SQL

**Bug fixed (NULL-safety):** The previous condition `NOT (x op col)` was not
NULL-safe: when `col IS NULL`, `NOT (x op NULL)` evaluates to NULL (not TRUE),
so the EXISTS clause did not fire for NULL rows, incorrectly including outer
rows that should have been excluded.

**New condition:** `(col IS NULL OR NOT (x op col))`
- Correctly excludes the outer row when any inner row has `col IS NULL`
- Correctly excludes the outer row when any inner row has `NOT (x op col)`
- Correctly includes the outer row when all inner rows satisfy `x op col`

**Example:**
```sql
SELECT * FROM orders WHERE price > ALL (SELECT threshold FROM limits);
-- AntiJoin condition: (threshold IS NULL OR NOT (price > threshold))
-- i.e. exclude order if any limit row has NULL threshold or price <= threshold
```

**Files changed:**
- `src/dvm/parser.rs` — `parse_all_sublink()`: updated condition to NULL-safe form

### Task 2.2: Deeply Nested SubLinks in OR (G4) — ✅ DONE

**Status:** **IMPLEMENTED** — the SubLinks-in-OR rewrite pipeline now handles
arbitrarily deep `AND(AND(... OR(EXISTS(...))))` nesting.

**Root cause of the gap:** `rewrite_and_with_or_sublinks()` previously iterated
only the direct children of the top-level AND node. `AND(a, AND(b, OR(EXISTS)))`
has only two direct children (`a` and `AND(b, OR(EXISTS))`), so the inner
`AND(b, OR(EXISTS))` was never recognized as "an OR-with-sublinks arm" —
causing the rewrite to silently skip the query, which then failed downstream.

**What was added:**
1. **`flatten_and_conjuncts(node, result)`** — new recursive helper that
   flattens `AND(a, AND(b, c))` → `[a, b, c]` at any nesting depth.
2. **`and_contains_or_with_sublink()`** updated to use `flatten_and_conjuncts`
   so it detects an OR-with-sublinks anywhere in a nested AND chain (not just
   one level deep).
3. **`rewrite_and_with_or_sublinks()`** signature changed from taking
   `and_expr: &BoolExpr` to `where_node: *mut Node`; body now calls
   `flatten_and_conjuncts` first, then finds the first OR-with-sublinks in the
   flat list.

**Before/after:**
```sql
-- Before (rejected — AND nesting was not flattened):
SELECT * FROM t WHERE x = 1 AND (y = 2 AND (z = 3 OR EXISTS (SELECT 1 FROM s WHERE s.id = t.id)))

-- After rewrite (supported):
SELECT * FROM t WHERE x = 1 AND y = 2 AND z = 3
UNION
SELECT * FROM t WHERE x = 1 AND y = 2 AND EXISTS (SELECT 1 FROM s WHERE s.id = t.id)
```

**Files changed:**
- `src/dvm/parser.rs`:
  - `flatten_and_conjuncts()` — new helper
  - `and_contains_or_with_sublink()` — uses `flatten_and_conjuncts`
  - `rewrite_and_with_or_sublinks()` — new signature + flattened body

### Task 2.3: ROWS FROM() with Multiple Functions (G5) — ✅ IMPLEMENTED

**Status:** **IMPLEMENTED** — `rewrite_rows_from()` in `src/dvm/parser.rs` detects
multi-function `ROWS FROM()` nodes in the raw parse tree and rewrites them.

Two strategies:
1. **All-unnest optimisation:** `ROWS FROM(unnest(A), unnest(B))` → `unnest(A, B) AS alias(cols)`.
   PostgreSQL's multi-arg `unnest()` natively does zip semantics.
2. **General case:** Ordinal-based LEFT JOIN LATERAL chain with
   `generate_series(1, 2147483647)` as index source + `row_number() OVER ()` for
   ordinal matching. Produces correct NULL-padding for shorter result sets.

Handles JoinExpr recursion (ROWS FROM inside JOIN trees), set operations (UNION
branches), aliases, column aliases, and WITH ORDINALITY.

**Files changed:**
- `src/dvm/parser.rs` — `rewrite_rows_from()` (public entry), `has_multi_rows_from()`,
  `rewrite_from_item_rows_from()`, `rewrite_rows_from_in_set_op()`
- `src/dvm/mod.rs` — export
- `src/api.rs` — wired into rewrite pipeline after `rewrite_sublinks_in_or`

**Tests:**
- 7 E2E tests in `tests/e2e_rows_from_tests.rs`: dual unnest, triple unnest,
  mixed SRFs, differential insert/delete, single-function passthrough

### Task 2.4: LATERAL with RIGHT/FULL JOIN (G6) — ✅ ERROR MESSAGE UPDATED

**Status:** **DONE** — error messages in `src/dvm/parser.rs` (two locations)
updated to explain that `RIGHT JOIN LATERAL` and `FULL JOIN LATERAL` are
rejected by PostgreSQL itself, not by pg_trickle. The messages now read:

```
LATERAL subqueries support only INNER JOIN and LEFT JOIN. RIGHT JOIN LATERAL
and FULL JOIN LATERAL are rejected by PostgreSQL itself because the lateral
reference on the right side creates a dependency that conflicts with
RIGHT/FULL JOIN semantics (got join type ...).
```

No further work planned for G6 — this is a PostgreSQL-level constraint
with no viable workaround.

---

## 6. Implementation Plan — Phase 3: Trigger-Level Optimisations

**Goal:** Reduce the write-path overhead of trigger-based CDC and improve
buffer table management. These changes are in `src/cdc.rs` and the PL/pgSQL
trigger functions — they do not affect the DVM engine or SQL coverage.

**Estimated effort:** 4–6 sessions (24–36 hours)

### Task 3.1: Column-Level Change Detection for UPDATE

**Status:** ✅ **DONE** —
`changed_cols BIGINT` column added to all new change buffer tables
(`create_change_buffer_table()`). `build_changed_cols_bitmask_expr()` generates
a per-column `IS DISTINCT FROM` bitmask expression. Both
`create_change_trigger()` and `rebuild_cdc_trigger_function()` write the bitmask
for UPDATE rows. `sync_change_buffer_columns()` recognises `changed_cols` as a
system column (not dropped on schema changes). `alter_change_buffer_add_columns()`
migrates existing buffer tables via `ADD COLUMN IF NOT EXISTS changed_cols BIGINT`.
All column values are still written (scan-layer optimisation is Task 3.4, deferred).

Deferred (Task 3.4 dependency): using the bitmask in `diff_scan_change_buffer()` to
skip unchanged columns requires a demand-propagation pass to prune `Scan.columns`
to only referenced columns. `resolve_columns()` currently returns ALL source columns;
this pass is deferred.

**Original behaviour:** The UPDATE trigger writes all NEW and OLD column
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

**Status:** ✅ **DONE** —
`execute_incremental_truncate_delete()` added to `src/refresh.rs`. The
TRUNCATE detection block now checks: if the ST has exactly one source AND the
current window contains no post-TRUNCATE rows (action `!= 'T'`), it calls
`execute_incremental_truncate_delete()` which issues a direct
`DELETE FROM stream_table` (O(ST rows) — no defining-query re-execution).
For multi-source STs or windows with post-TRUNCATE inserts, it falls back to
`execute_full_refresh()` unchanged.

**Original behaviour:** TRUNCATE on a source table writes a 'T' marker →
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

**Status: ✅ IMPLEMENTED**

**Current behaviour:** Change buffer tables are unpartitioned heap tables.
After each refresh cycle, consumed rows are deleted via
`DELETE FROM changes_<oid> WHERE lsn <= frontier`. VACUUM must reclaim dead
tuples.

**Implementation:** Buffer tables can now use `PARTITION BY RANGE (lsn)`:

1. `pg_trickle.buffer_partitioning` GUC: `off` (default) / `on` / `auto`.
2. `create_change_buffer_table()` adds `PARTITION BY RANGE (lsn)` clause and
   creates a default partition when partitioning is enabled.
3. `should_auto_partition()` checks if the minimum schedule across dependent
   stream tables is ≥ 30s.
4. `is_buffer_partitioned()` checks `relkind = 'p'` in `pg_class`.
5. `create_cycle_partition()` creates a range partition for each refresh cycle.
6. `detach_consumed_partitions()` finds child partitions via `pg_inherits`,
   parses bounds, and detaches + drops consumed partitions (O(1), no VACUUM).
7. `drain_pending_cleanups()` and `cleanup_change_buffers_by_frontier()` in
   `refresh.rs` check `is_buffer_partitioned()` and use partition detach path.

### Task 3.4: Skip-Unchanged-Column Scanning in Delta

**Status: ✅ IMPLEMENTED**

**Implementation:** `prune_scan_columns()` post-parse pass in `parser.rs`:

1. `ColumnRefSet` collects all `Expr::ColumnRef` from the entire OpTree,
   tracking both qualified (`alias.column`) and unqualified column names.
2. `collect_all_column_refs()` walks the tree recursively. Returns `false`
   (bail out) on `Star`, `Raw`, `LateralFunction`, or `LateralSubquery`.
3. `apply_column_pruning()` walks the tree; at each `Scan` node, retains
   only columns in `demanded ∪ pk_columns`.
4. Hook in `parse_defining_query_inner()` after tree construction, plus
   loop over CTE registry entries.
5. 8 unit tests covering qualified/unqualified refs, PK retention, Star/Raw
   bail-out, filter refs, join conditions, and aggregates.

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

**Status:** ✅ **DONE** — Already fully wired in a prior session. `AggFunc::Corr`, `CovarPop`, `CovarSamp`, `RegrAvgx`, `RegrAvgy`, `RegrCount`, `RegrIntercept`, `RegrR2`, `RegrSlope`, `RegrSxx`, `RegrSxy`, `RegrSyy` variants exist in `src/dvm/parser.rs`; all are in the `is_group_rescan()` match arm, have correct `sql_name()` mappings, and are wired in the `extract_aggregates` match block. G7 closed.

### Task 4.2: Hypothetical-Set Aggregates (G8) — 4 Functions

**Status:** ✅ **DONE** — `AggFunc::HypRank`, `HypDenseRank`, `HypPercentRank`, `HypCumeDist` variants added to `src/dvm/parser.rs`; all routed through `is_group_rescan()` = true; `is_ordered_set` in `agg_to_rescan_sql` updated to include all four; match arm wired ("rank" / "dense_rank" / "percent_rank" / "cume_dist" → respective variants); `sql_name()` returns `"RANK"` / `"DENSE_RANK"` / `"PERCENT_RANK"` / `"CUME_DIST"`. G8 closed.

### Task 4.3: XMLAGG (G9)

**Status:** ✅ **DONE** — `AggFunc::XmlAgg` variant added to `src/dvm/parser.rs`; `sql_name()` → `"XMLAGG"`; `is_group_rescan()` = true (ORDER BY is passed through inside the call, not via `is_ordered_set`); match arm wired ("xmlagg" → `AggFunc::XmlAgg`). G9 closed.

---

## 8. Implementation Plan — Phase 5: IMMEDIATE Mode Parity

**Goal:** Extend IMMEDIATE mode to support constructs currently rejected
by `validate_immediate_mode_support()`, closing the gap with DIFFERENTIAL
mode.

**Estimated effort:** 2–3 sessions (12–18 hours)

### Task 5.1: Recursive CTEs in IMMEDIATE Mode (G10)

**Status:** ✅ **Done**

**Current status:** ~~`RecursiveCte` is the only OpTree node rejected by
`check_immediate_support()`.~~ Resolved — `RecursiveCte` is now allowed
with a warning about potential stack-depth issues.

**Root cause:** Recursive CTEs use semi-naive evaluation with fixpoint
iteration. In IMMEDIATE mode, the delta source is a transition table (temp
table), not a change buffer. The concern is that fixpoint iteration within
a trigger function may:
1. Exceed PostgreSQL's `max_stack_depth` for deeply recursive CTEs.
2. Interact incorrectly with the trigger's transaction state (SPIs, snapshots).

**Implementation:**

- `check_immediate_support()` in `src/dvm/parser.rs` changed from returning
  `Err(UnsupportedOperator(...))` to emitting a `pgrx::warning!()` about stack
  depth and recursing into `base` and `recursive` fields for validation.
- The existing semi-naive evaluation path with `DeltaSource::TransitionTable`
  proceeds as before.

**Files changed:**
- `src/dvm/parser.rs` — changed `RecursiveCte` arm from rejection to warning

**Tests:**
- E2E test: create IMMEDIATE stream table with `WITH RECURSIVE`, INSERT
  into base table, verify stream table is updated correctly within the
  same transaction

### Task 5.2: TopK in IMMEDIATE Mode (G11)

**Status:** ✅ **Done**

**Current status:** ~~TopK (`ORDER BY + LIMIT`) is rejected in IMMEDIATE mode~~
Resolved — TopK is now supported in IMMEDIATE mode with a limit threshold guard.

**Root cause:** TopK uses a different delta strategy than standard
differential: instead of delta algebra, it maintains a bounded result set
via scoped recomputation (delete rows that fall outside the top K, insert
rows that enter it). This is orchestrated by the refresh engine, not the
trigger function.

**Implementation:** Statement-level TopK maintenance in the IVM trigger:

1. `apply_topk_micro_refresh()` in `src/ivm.rs` materializes the new top K
   into a temp table using the defining query with LIMIT.
2. DELETE rows from stream table that are no longer in top K.
3. INSERT ON CONFLICT for rows that entered or changed in the top K.
4. Uses `row_id_expr_for_query()` and `get_defining_query_columns()` from DVM.

**GUC guard:** `pg_trickle.ivm_topk_max_limit` (default 1000). TopK queries
with LIMIT > this threshold are rejected at creation/alter time.

**Files changed:**
- `src/api.rs` — threshold check replaces hard rejection
- `src/ivm.rs` — `apply_topk_micro_refresh()` added
- `src/config.rs` — `PGS_IVM_TOPK_MAX_LIMIT` GUC

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
- [x] Buffer table partitioning eliminates vacuum overhead for high-write
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
  ├─ Task 3.3: Buffer table partitioning    → ✅ Done — partition by LSN range + GUC
  ├─ Task 3.4: Skip-unchanged-col scanning  → ✅ Done — prune_scan_columns() pass
  └─ Task 3.5: Online ADD COLUMN            → Avoids unnecessary full reinit

Phase 4 — Remaining Aggregates               (2–3 sessions, 12–18 hours)
  ├─ Task 4.1: Regression aggregates (11)   → Statistical analysis coverage
  ├─ Task 4.2: Hypothetical-set aggs (4)    → Completes ordered-set coverage
  └─ Task 4.3: XMLAGG                       → Niche, trivial

Phase 5 — IMMEDIATE Mode Parity             ✅ COMPLETE
  ├─ Task 5.1: Recursive CTEs              → ✅ Done — warning instead of rejection
  └─ Task 5.2: TopK                         → ✅ Done — micro-refresh + GUC guard

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
