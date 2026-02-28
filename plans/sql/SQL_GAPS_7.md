# PLAN: SQL Gaps — Phase 7

**Status:** In Progress  
**Date:** 2026-02-25  
**Branch:** `main`  
**Scope:** Deep gap analysis — PostgreSQL 18 SQL coverage, operational correctness, delta computation edge cases, test coverage, and production readiness.  
**Current state:** ~920 unit tests, 23 E2E test suites (384 E2E tests), 74 integration tests, 39 AggFunc variants, 22 OpTree variants (21 unique diff operators), 6 auto-rewrite passes, 20 GUC variables, 13,108-line DVM parser, 40,094 total source lines.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Methodology](#2-methodology)
3. [Ground Truth: Current Implementation State](#3-ground-truth-current-implementation-state)
4. [Gap Category 1: Delta Computation Correctness](#4-gap-category-1-delta-computation-correctness)
5. [Gap Category 2: WAL Decoder Correctness](#5-gap-category-2-wal-decoder-correctness)
6. [Gap Category 3: DDL Event Tracking Gaps](#6-gap-category-3-ddl-event-tracking-gaps)
7. [Gap Category 4: Refresh Engine Edge Cases](#7-gap-category-4-refresh-engine-edge-cases)
8. [Gap Category 5: SQL Syntax & Expression Gaps](#8-gap-category-5-sql-syntax--expression-gaps)
9. [Gap Category 6: Test Coverage Gaps](#9-gap-category-6-test-coverage-gaps)
10. [Gap Category 7: CDC & Change Buffer Issues](#10-gap-category-7-cdc--change-buffer-issues)
11. [Gap Category 8: Production & Deployment Concerns](#11-gap-category-8-production--deployment-concerns)
12. [Gap Category 9: Monitoring & Observability](#12-gap-category-9-monitoring--observability)
13. [Gap Category 10: Intentional Rejections (Confirmed OK)](#13-gap-category-10-intentional-rejections-confirmed-ok)
14. [Prioritized Implementation Roadmap](#14-prioritized-implementation-roadmap)
15. [Historical Progress Summary](#15-historical-progress-summary)

---

## 1. Executive Summary

pg_trickle's SQL coverage is now **mature**: every PostgreSQL SELECT
construct is either handled correctly or rejected with a clear, actionable
error message. Phases 1–6 resolved 75+ original gaps, implemented 6
auto-rewrite passes, 22 OpTree variants, and 39 aggregate functions, all
with zero remaining P0 or P1 issues in the core SQL parser.

This Phase 7 analysis shifts focus from SQL syntax coverage (largely complete)
to **second-order correctness** — the behaviors that emerge when real-world
workloads hit edge cases in delta computation, CDC, the WAL decoder, DDL
tracking, and production deployment:

| Category | P0 | P1 | P2 | P3 | P4 | Total |
|----------|----|----|----|----|----|----|
| Delta computation correctness | 0 | 3 | 0 | 1 | 2 | 6 |
| WAL decoder correctness | 0 | 3 | 0 | 2 | 0 | 5 |
| DDL event tracking | 0 | 3 | 0 | 2 | 1 | 6 |
| Refresh engine edge cases | 1 | 0 | 1 | 3 | 1 | 6 |
| SQL syntax & expression gaps | 0 | 0 | 2 | 2 | 2 | 6 |
| Test coverage gaps | 0 | 0 | 0 | 8 | 0 | 8 |
| CDC & change buffer issues | 0 | 1 | 0 | 1 | 2 | 4 |
| Production & deployment | 0 | 1 | 1 | 4 | 1 | 7 |
| Monitoring & observability | 0 | 0 | 0 | 4 | 1 | 5 |
| **Total new gaps** | **1** | **11** | **4** | **27** | **10** | **53** |

The single P0 item is:
1. **Remove `delete_insert` merge strategy** — the strategy is unsafe for
   aggregate/DISTINCT queries (double-evaluation against mutated state), slower
   than `MERGE` for small deltas, and incompatible with prepared statements.
   The `auto` strategy already covers the only legitimate use case (large-delta
   bulk apply). Decision: remove `delete_insert` as a valid GUC value and emit
   an error if it is set.

The 11 P1 items cluster in three areas:
- **WAL decoder** (3): pk_hash=0 for keyless tables, `old_*` columns always NULL
  on UPDATE, naive pgoutput action string parsing
- **Delta computation** (3): JOIN key column changes with simultaneous right-side
  changes, window function partition key changes, recursive CTE non-monotone
  semi-naive divergence
- **DDL tracking** (3): untracked `ALTER TYPE`, `ALTER DOMAIN`, and `ALTER POLICY`
  changes affecting source table columns
- **Production** (1): PgBouncer transaction-mode pooling incompatibility with
  session-level advisory locks and prepared statements
- **CDC** (1): keyless table content-hash collision when rows have identical content

**Key observation:** All 3 WAL decoder P1 issues are dormant — they only manifest
when the WAL CDC path is active (triggered by `pg_trickle.wal_enabled = true` and
a successful transition from triggers). The default trigger-based CDC is not
affected. However, these must be fixed before the WAL path is promoted to
production-ready.

---

## 2. Methodology

This analysis was performed by:

1. **Full source-code audit** of all 40,094 lines across 27 Rust source files:
   - `src/dvm/parser.rs` (13,108 lines) — every `node_to_expr()` arm, rejection
     function, and auto-rewrite pass
   - `src/dvm/operators/*.rs` (12,487 lines across 22 files) — every diff
     operator, delta SQL generation, and edge case comment
   - `src/refresh.rs` — MERGE template generation, differential/full refresh,
     fallback paths, prepared statements, user-trigger DML
   - `src/hooks.rs` — DDL event trigger handling, schema change classification
   - `src/cdc.rs` — trigger creation, change buffer management, column resolution
   - `src/wal_decoder.rs` — WAL-based CDC, pgoutput parsing, transition logic
   - `src/catalog.rs` — catalog CRUD, frontier management, crash recovery
   - `src/dag.rs` — dependency graph, topological sort, cascade
   - `src/hash.rs` — row identity hashing, collision analysis
   - `src/scheduler.rs` — background worker, scheduling logic
   - `src/monitor.rs` — monitoring functions, health checks, alerts

2. **E2E test coverage analysis** of 384 E2E tests across 23 test files,
   cross-referenced against all implemented features to identify untested code
   paths

3. **Delta computation edge case analysis** — systematic review of each diff
   operator for correctness under simultaneous multi-table changes, NULL
   handling, key column mutations, and empty result sets

4. **PostgreSQL 18 feature cross-reference** — comparison of PG 18 release notes
   and SQL:2023 features against handled parse tree nodes

5. **Production deployment simulation** — analysis of interaction with connection
   poolers, read replicas, extension upgrades, and memory pressure

6. **Review of all previous gap analyses** (SQL_GAPS_1 through SQL_GAPS_6) —
   verification that all previously-marked items are either resolved or
   correctly tracked

---

## 3. Ground Truth: Current Implementation State

### 3.1 Supported Features (DIFFERENTIAL mode)

| Category | Features | Count |
|----------|----------|-------|
| **OpTree variants** | Scan, Project, Filter, InnerJoin, LeftJoin, FullJoin, Aggregate, Distinct, UnionAll, Intersect, Except, Subquery, CteScan, RecursiveCte, RecursiveSelfRef, Window, LateralFunction, LateralSubquery, SemiJoin, AntiJoin, ScalarSubquery | 22 |
| **Diff operators** | scan, filter, project, join, outer_join, full_join, aggregate, distinct, union_all, intersect, except, subquery, cte_scan, recursive_cte, window, lateral_function, lateral_subquery, semi_join, anti_join, scalar_subquery, join_common | 21 |
| **Aggregate functions** | COUNT/COUNT(\*), SUM, AVG, MIN, MAX, BOOL_AND/EVERY, BOOL_OR, STRING_AGG, ARRAY_AGG, JSON_AGG, JSONB_AGG, JSON_OBJECT_AGG, JSONB_OBJECT_AGG, JSON_OBJECTAGG_STD, JSON_ARRAYAGG_STD, BIT_AND, BIT_OR, BIT_XOR, STDDEV_POP/STDDEV, STDDEV_SAMP, VAR_POP, VAR_SAMP/VARIANCE, MODE, PERCENTILE_CONT, PERCENTILE_DISC, ANY_VALUE, CORR, COVAR_POP, COVAR_SAMP, REGR_AVGX/AVGY/COUNT/INTERCEPT/R2/SLOPE/SXX/SXY/SYY | 39 |
| **Expression types** | ColumnRef, Literal, BinaryOp (AEXPR_OP + unary), BoolExpr (AND/OR/NOT), FuncCall, TypeCast, NullTest, CaseExpr, CoalesceExpr, NullIfExpr, MinMaxExpr, SQLValueFunction (11 variants), BooleanTest (6 variants), SubLink (EXISTS/ANY/EXPR), ArrayExpr, RowExpr, A_Indirection, AEXPR_IN, AEXPR_BETWEEN (4 variants), AEXPR_DISTINCT/NOT_DISTINCT, AEXPR_SIMILAR, AEXPR_OP_ANY/ALL, CollateClause, JsonIsPredicate, JsonObjectConstructor, JsonArrayConstructor, JsonArrayQueryConstructor, JsonParseExpr, JsonScalarExpr, JsonSerializeExpr, JsonObjectAgg, JsonArrayAgg | 40+ |
| **Auto-rewrites** | View inlining, DISTINCT ON → ROW_NUMBER(), GROUPING SETS/CUBE/ROLLUP → UNION ALL, Scalar subquery in WHERE → CROSS JOIN, SubLinks in OR → UNION, Multi-PARTITION BY windows → nested subqueries | 6 |
| **Join types** | INNER, LEFT, RIGHT (→LEFT swap), FULL, CROSS, NATURAL (catalog-resolved), LATERAL (INNER/LEFT) | 7 |
| **Set operations** | UNION ALL, UNION (dedup), INTERSECT [ALL], EXCEPT [ALL], mixed UNION/UNION ALL | 5 |
| **CTEs** | Non-recursive (single + multi-ref shared delta), WITH RECURSIVE (semi-naive/DRed/recomputation) | 2 |
| **Window** | PARTITION BY, ORDER BY, frame clauses (ROWS/RANGE/GROUPS + EXCLUDE), named WINDOW | Full |
| **Subqueries** | FROM subquery, EXISTS/NOT EXISTS in WHERE, IN/NOT IN (subquery), ALL (subquery), scalar in SELECT, scalar in WHERE (rewritten) | Full |

### 3.2 Auto-Rewrite Pipeline (6 passes)

| Order | Function | Trigger | Rewrite |
|-------|----------|---------|---------|
| 0 | `rewrite_views_inline()` | FROM references a view (`relkind = 'v'`) | → inline subquery; iterative for nested views |
| 1 | `rewrite_distinct_on()` | `DISTINCT ON(expr)` in SELECT | → `ROW_NUMBER() OVER (PARTITION BY expr ORDER BY ...) = 1` subquery |
| 2 | `rewrite_grouping_sets()` | `GROUPING SETS`/`CUBE`/`ROLLUP` in GROUP BY | → `UNION ALL` of separate `GROUP BY` queries with `GROUPING()` literals |
| 3 | `rewrite_scalar_subquery_in_where()` | Scalar `SubLink` in WHERE | → `CROSS JOIN` with scalar subquery as inline view |
| 4 | `rewrite_sublinks_in_or()` | `EXISTS`/`IN` SubLinks inside `OR` | → `UNION` of separate filtered queries |
| 5 | `rewrite_multi_partition_windows()` | Multiple different `PARTITION BY` clauses | → Nested subqueries, one per distinct partitioning |

### 3.3 Explicitly Rejected Constructs

| Construct | Error Behavior |
|-----------|---------------|
| LIMIT / OFFSET / FETCH FIRST | Rejected — stream tables need all rows |
| FOR UPDATE / FOR SHARE | Rejected — row-level locking incompatible |
| TABLESAMPLE | Rejected — non-deterministic |
| ALL sublink (`x op ALL (SELECT ...)`) | Rejected — use NOT EXISTS |
| ROWS FROM() with multiple functions | Rejected — use single SRF |
| Materialized views as DIFF source | Rejected — no CDC mechanism |
| Foreign tables as DIFF source | Rejected — no CDC mechanism |
| User-defined aggregates | Rejected — only built-in PG aggregates |
| Window functions nested in expressions | Rejected — must be top-level SELECT |
| XMLAGG | Rejected — extremely niche |
| Hypothetical-set aggregates (as aggregates) | Rejected — use as window functions |

---

## 4. Gap Category 1: Delta Computation Correctness

These are edge cases in the diff operators where the delta SQL can produce
semantically incorrect results under specific data mutation patterns.

### G1.1 — JOIN Key Column Change with Simultaneous Right-Side Changes

| Field | Value |
|-------|-------|
| **Operator** | `diff_inner_join`, `diff_left_join` |
| **Scenario** | Row's join key is updated (`UPDATE orders SET cust_id = 5 WHERE cust_id = 3`) in the same refresh cycle as the old customer (id=3) is deleted from the right table |
| **Delta behavior** | The scan emits DELETE(old) + INSERT(new). The JOIN delta Part 1 (`delta_left JOIN current_right`) joins the DELETE(old, cust_id=3) against `current_right` — but customer 3 was deleted, so the DELETE finds no match and is dropped. The stream table retains the stale join result for (cust_id=3). |
| **Severity** | **P1 — Incorrect data retention** |
| **Impact** | Medium — requires simultaneous key change + related row deletion in same refresh |
| **Root cause** | The delta query reads `current_right` after all changes are applied (snapshot at query time), not at the frontier LSN |
| **Effort** | 8–12 hours |

**Options:**

**A. CTE snapshot** — wrap the right side in a CTE that reads at the frontier LSN using `pg_snapshot_xmin()`
- ✅ Fully correct under simultaneous multi-table mutations
- ❌ Requires SERIALIZABLE isolation or snapshot export — invasive and expensive
- ❌ Complex implementation; risk of introducing other timing edge cases

**B. Dual-phase delta** — compute DELETE delta first using old state, then INSERT delta using new state
- ✅ Architecturally clean separation of old and new state
- ❌ Doubles the delta query execution cost on every refresh
- ❌ Architecturally invasive — requires significant rework of the refresh pipeline

**C. Compensating anti-join** — after the MERGE, detect orphaned rows whose join partner no longer exists and emit corrective DELETEs
- ✅ Simpler than A or B — a clean-up pass after the existing MERGE
- ✅ Correct for the specific failure mode (stale join result retention)
- ❌ Adds a post-MERGE pass on every refresh, even for unaffected tables
- ❌ Does not handle all races — only the "partner deleted" case

**D. Document + FULL fallback** — document the edge case; rely on the adaptive threshold to trigger FULL refresh when large batches of key-modifying UPDATEs are detected
- ✅ Zero code change — no implementation risk
- ✅ The adaptive threshold already handles bulk-UPDATE workloads naturally
- ✅ The failure mode requires a very specific simultaneous combination (key change + right-side delete in the same cycle) — rare in practice
- ❌ Does not protect against the bug in low-frequency, surgical update scenarios
- ❌ Relies on the user understanding the documented limitation

**Decision:** Option D for v0.2.0 (document + rely on adaptive FULL fallback). Option C as a future enhancement if customer reports surface.

---

### G1.2 — Window Function Partition Key Changes

| Field | Value |
|-------|-------|
| **Operator** | `diff_window` |
| **Scenario** | `UPDATE` changes a column used in `PARTITION BY`. The row moves from partition A to partition B. |
| **Delta behavior** | The window operator uses partition-based recomputation: it re-evaluates the entire partition for any partition that contains changed rows. The scan emits DELETE(old) + INSERT(new). The old partition-key value triggers recomputation of partition A; the new value triggers recomputation of partition B. |
| **Current handling** | The scan's DELETE+INSERT splitting should propagate correctly if the delta query references `old_*` columns for the DELETE half. The DELETE carries the old partition key → partition A is recomputed without the moved row. The INSERT carries the new partition key → partition B is recomputed with the moved row. |
| **Severity** | **P1 — Potential incorrect partition membership** |
| **Impact** | Medium — requires UPDATE on PARTITION BY key column |
| **Effort** | 4–6 hours (verification + E2E test) |

**Options (test-gated):** Write a targeted E2E test that UPDATEs a PARTITION BY key and verifies both old and new partitions are correct after refresh.

- **If test passes:** downgrade to P4 (documented edge case, no fix needed)
  - ✅ Zero implementation cost
  - ✅ Confirms the existing DELETE+INSERT splitting is already correct
- **If test fails — fix `diff_window`:** adapt the window operator to snapshot the old partition key before applying the delta
  - ✅ Correct for all PARTITION BY key change scenarios
  - ❌ 4–6h implementation inside the most complex diff operator
- **If test fails — document as known limitation:**
  - ✅ Zero code change
  - ❌ Any UPDATE on a PARTITION BY key produces silently wrong results

**Decision:** Test-gated — run E2E first, then decide.

---

### G1.3 — Recursive CTE Non-Monotone Semi-Naive Divergence

| Field | Value |
|-------|-------|
| **Operator** | `diff_recursive_cte` |
| **Scenario** | A recursive CTE with `EXCEPT` or aggregation in the recursive term |
| **Delta behavior** | Semi-naive evaluation iterates the recursive term with only new rows. For non-monotone operators (EXCEPT, NOT EXISTS, aggregation), the fixpoint computed incrementally may differ from the fixpoint computed from scratch. |
| **Current handling** | Recursive CTEs in DIFFERENTIAL mode use one of three strategies: semi-naive, DRed (delete-and-rederive), or full recomputation. The strategy selection may not correctly detect non-monotone recursive terms in all cases. |
| **Severity** | **P1 — Potential incorrect fixpoint** |
| **Impact** | Low — non-monotone recursive CTEs are rare |
| **Effort** | 6–8 hours |

**Options (audit-gated):** Audit the monotonicity detection in `diff_recursive_cte` and add E2E tests with non-monotone recursive queries (e.g., `WITH RECURSIVE ... SELECT ... EXCEPT SELECT ...`).

- **If audit confirms correct fallback:** downgrade to P4 (verified by test)
  - ✅ No code change needed
  - ✅ Non-monotone recursive CTEs are rare in practice
- **If audit finds incorrect fallback — fix strategy selector:**
  - ✅ Correct differential behaviour for all recursive CTE variants
  - ❌ The fixpoint logic in `diff_recursive_cte` is complex; risk of regression
- **If audit finds incorrect fallback — reject non-monotone recursive CTEs in DIFFERENTIAL mode:**
  - ✅ Simple guard: detect EXCEPT/NOT EXISTS/aggregation in recursive term and reject with a clear error
  - ✅ Users are directed to FULL mode, which is always correct
  - ❌ Reduces SQL coverage for an edge case that is theoretically supportable

**Decision:** Audit-gated — run audit + E2E first, then decide between fix and rejection.

---

### G1.4 — Aggregate HAVING Group Transitions

| Field | Value |
|-------|-------|
| **Operator** | `diff_aggregate` (group-rescan path) |
| **Scenario** | A GROUP BY group transitions from satisfying to not satisfying the HAVING predicate (or vice versa) due to changed data |
| **Delta behavior** | Group-rescan re-evaluates the full `SELECT ... GROUP BY ... HAVING ...` pipeline per affected group. If the HAVING now excludes a previously included group, the rescan should emit a DELETE for that group. If the HAVING now includes a previously excluded group, it should emit an INSERT. |
| **Current handling** | The group-rescan reads from the source table. If the rescan returns no rows for a group (because HAVING excludes it), the MERGE's `WHEN NOT MATCHED BY SOURCE` arm should DELETE the stale group row. |
| **Severity** | **P3 — Likely correct but unverified** |
| **Impact** | Low — HAVING clause transitions are uncommon |
| **Effort** | 2–3 hours (E2E test) |

**Recommendation:** Write E2E test: create ST with `HAVING COUNT(*) > 2`, insert
3 rows in group, verify group appears, delete 1 row, refresh, verify group
disappears.

---

### G1.5 — Keyless Table Row-ID Collision with Duplicate Rows

| Field | Value |
|-------|-------|
| **Operator** | `diff_scan` (keyless table path) |
| **Scenario** | Table without PRIMARY KEY has two identical rows. Content-hash `__pgt_row_id` is the same for both. |
| **Delta behavior** | Both rows hash to the same `__pgt_row_id`. The MERGE treats them as one row. Inserting a duplicate appears as no change. Deleting one of two duplicates may delete both (the DELETE delta matches on `__pgt_row_id` which hits both). |
| **Severity** | **P4 — Edge case for keyless tables with duplicates** |
| **Impact** | Very low — keyless tables + exact duplicate rows is unusual |
| **Effort** | 2–3 hours (document + E2E test) |

**Recommendation:** Document that keyless tables with duplicate rows may produce
incorrect delta results. Recommend users add a PRIMARY KEY or at least a UNIQUE constraint.

---

### G1.6 — FULL JOIN Delta with NULL Keys on Both Sides

| Field | Value |
|-------|-------|
| **Operator** | `diff_full_join` |
| **Scenario** | Both left and right tables have rows with NULL join-key values. After delta application, the FULL JOIN must preserve NULL-keyed rows from both sides. |
| **Current handling** | `diff_full_join` computes the delta as `(delta_L ⋈ R) ∪ (L' ⋈ delta_R) ∪ unmatched_delta_L ∪ unmatched_delta_R`. The unmatched portions use `NOT EXISTS` anti-semi-joins, which correctly handle NULL keys (NULL ≠ NULL → always unmatched). |
| **Severity** | **P4 — Likely correct but unverified** |
| **Impact** | Very low — FULL JOIN with NULL keys on both sides is rare |
| **Effort** | 2 hours (E2E test) |

**Recommendation:** Write E2E test for FULL JOIN with NULL join keys to confirm
correctness.

---

## 5. Gap Category 2: WAL Decoder Correctness

These gaps only manifest when the WAL CDC path is active
(`pg_trickle.wal_enabled = true`). The default trigger-based CDC is unaffected.

### G2.1 — pk_hash Returns "0" for Keyless Tables

| Field | Value |
|-------|-------|
| **Location** | `wal_decoder.rs:build_pk_hash_from_values()` |
| **Problem** | When `pk_columns` is empty (keyless table), the WAL decoder returns literal `"0"` for pk_hash. The trigger-based CDC path computes `pg_trickle_hash(row_to_json(NEW)::text)`. This mismatch means the same physical row gets different `pk_hash` values depending on CDC mode. |
| **Impact** | During TRIGGER→WAL transition, WAL-mode rows have `pk_hash = 0` while existing rows have content-based hashes. The MERGE fails to match, causing duplicate rows in the stream table. |
| **Severity** | **P1 — Silent duplicates** |
| **Effort** | 4–6 hours |

**Options:**

**A. Implement all-column content hashing in WAL decoder**
- ✅ WAL CDC works transparently for keyless tables — no user-facing restriction
- ✅ Consistent hash between trigger and WAL paths, enabling clean TRIGGER→WAL transition
- ❌ Hashing all columns on every row is CPU-expensive for wide tables
- ❌ Must reproduce the exact serialisation of `row_to_json(NEW)::text` from raw WAL bytes — fragile
- ❌ Generated or excluded columns must be aligned precisely between both paths, or transitions produce hash mismatches

**B. Require PRIMARY KEY for WAL mode**
- ✅ Simple: one guard check at transition time with a clear error message
- ✅ Avoids hashing performance overhead
- ✅ Any table worth production WAL CDC should have a PK; a reasonable prerequisite
- ❌ Permanent capability gap — keyless tables stay on trigger-based CDC
- ❌ Creates an asymmetry: trigger path supports keyless tables, WAL path does not

**Decision:** Option B — require PRIMARY KEY for WAL mode. The WAL path is opt-in and production-gated; requiring a PK is a defensible prerequisite. The asymmetry is acceptable and clearly documented.

---

### G2.2 — UPDATE old_\* Columns Always NULL

| Field | Value |
|-------|-------|
| **Location** | `wal_decoder.rs` UPDATE event handling |
| **Problem** | For UPDATE events, `old_*` columns are always written as NULL (comment: "simplified here"). The trigger-based path writes the actual old values. |
| **Impact** | This breaks the scan delta's UPDATE→DELETE+INSERT splitting, which uses `old_*` values for the DELETE half. Filter predicates that detect rows leaving a filter boundary also need old values. Any non-trivial defining query produces incorrect deltas in WAL CDC mode. |
| **Severity** | **P1 — Incorrect deltas for all non-trivial queries** |
| **Effort** | 8–12 hours |

**Options:**

**A. Require `REPLICA IDENTITY FULL` on source tables**
- ✅ Gives the complete old tuple — exactly mirrors what triggers capture
- ✅ Simpler decoder: just parse the pgoutput `O` (old tuple) message
- ✅ Correct for all query types: filter boundary detection, non-PK joins, aggregations
- ❌ Doubles WAL volume per UPDATE on every source table — significant on write-heavy workloads
- ❌ Requires `ALTER TABLE ... REPLICA IDENTITY FULL` on each source table — user-visible setup step
- ❌ Easy to miss on newly added source tables; no automatic enforcement

**B. Handle `REPLICA IDENTITY DEFAULT` (PK columns only)**
- ✅ No WAL size overhead for tables that don't need full old-row data
- ✅ Works correctly for simple cases: DELETEs and UPDATEs that don't cross filter boundaries
- ❌ `old_*` values only available for PK columns — non-PK old values remain NULL
- ❌ Any stream table whose delta depends on old non-PK values produces silently wrong results
- ❌ Significantly more complex: must track which columns have old values per event
- ❌ Partial coverage is very difficult to explain and reason about

**Decision:** Option A — require `REPLICA IDENTITY FULL`. Partial old-tuple coverage (B) creates a category of silent wrong results worse than the trigger fallback. Requiring `REPLICA IDENTITY FULL` is a documented, one-time setup step and standard practice for logical replication use cases.

---

### G2.3 — pgoutput Action Parsing is Naive String-Contains

| Field | Value |
|-------|-------|
| **Location** | `wal_decoder.rs:parse_pgoutput_action()` |
| **Problem** | Action detection uses `data.contains("INSERT")`, `data.contains("UPDATE")`, etc. A table named `INSERT_LOG` or a text column value containing "DELETE" would misparse the action. |
| **Severity** | **P1 — Silent misclassification** |
| **Impact** | Medium — any table or column value containing action keywords |
| **Effort** | 2–3 hours |

**Decision:** Parse the pgoutput format positionally — the action keyword appears after `table <schema>.<table>: <ACTION>:`. Use a regex or positional parsing instead of `contains()`. No real alternative: the current approach is a bug, not a design choice.

---

### G2.4 — WAL Transition Timeout with No Automatic Retry

| Field | Value |
|-------|-------|
| **Location** | `wal_decoder.rs`, `config.rs:PGS_WAL_TRANSITION_TIMEOUT` |
| **Problem** | If the WAL transition times out (default 300s), it aborts and reverts to triggers. No automatic retry. Requires scheduler restart or manual GUC reset for next attempt. |
| **Severity** | **P3** |
| **Effort** | 3–4 hours |

**Recommendation:** Implement exponential-backoff retry for WAL transitions.

---

### G2.5 — Column Rename Detection in WAL Mode

| Field | Value |
|-------|-------|
| **Location** | `wal_decoder.rs:detect_schema_mismatch()` |
| **Problem** | Only detects new columns (more decoded than expected). Column renames (same count, different names) are not detected — values would be decoded into wrong column names in the buffer table. |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

**Recommendation:** Compare column names, not just column count.

---

## 6. Gap Category 3: DDL Event Tracking Gaps

The DDL event trigger system in `hooks.rs` tracks `ALTER TABLE` on source tables
and classifies changes as Benign/ConstraintChange/ColumnChange. However, several
DDL event types that can affect stream table semantics are not tracked.

### G3.1 — ALTER TYPE Not Tracked

| Field | Value |
|-------|-------|
| **Location** | `hooks.rs` — `ddl_command_end` handler |
| **Problem** | `ALTER TYPE` (e.g., adding an enum value, renaming an enum value, modifying a composite type) is not tracked. If a column uses an enum type and `ALTER TYPE ... RENAME VALUE` is executed, the stream table's defining query may produce different results. |
| **Severity** | **P1 — Silent semantic drift** |
| **Impact** | Low — enum value rename is uncommon |
| **Effort** | 3–4 hours |

**Recommendation:** Track `ALTER TYPE` events. For `ADD VALUE`: benign (new values
don't invalidate existing data). For `RENAME VALUE`: reinitialize affected STs.

---

### G3.2 — ALTER DOMAIN Not Tracked

| Field | Value |
|-------|-------|
| **Location** | `hooks.rs` |
| **Problem** | If a source column uses a domain type and `ALTER DOMAIN` adds a constraint, the next refresh could fail if the delta INSERT violates the new domain constraint. The ST is not proactively invalidated. |
| **Severity** | **P1 — Unexpected refresh failure** |
| **Impact** | Very low — domain types in stream table sources are rare |
| **Effort** | 2–3 hours |

**Recommendation:** Track `ALTER DOMAIN` events and reinitialize STs that
reference columns of the affected domain type.

---

### G3.3 — ALTER POLICY / RLS Changes Not Tracked

| Field | Value |
|-------|-------|
| **Location** | `hooks.rs` |
| **Problem** | Row-Level Security (RLS) policy changes can silently alter the result set of the defining query. If the background worker's role is subject to RLS policies, a policy change can cause the stream table to silently include or exclude rows. |
| **Severity** | **P1 — Silent result set change** |
| **Impact** | Low — RLS on source tables + stream tables is an advanced combination |
| **Effort** | 3–4 hours |

**Recommendation:** Track `CREATE POLICY`, `ALTER POLICY`, `DROP POLICY`, and
`ALTER TABLE ... ENABLE/DISABLE ROW LEVEL SECURITY`. Reinitialize affected STs.

---

### G3.4 — GRANT/REVOKE on Source Tables Not Tracked

| Field | Value |
|-------|-------|
| **Location** | `hooks.rs` |
| **Problem** | If the background worker's role loses `SELECT` on a source table, refresh fails with a cryptic SPI permission error. No proactive detection or clear error. |
| **Severity** | **P3** |
| **Impact** | Low — permission changes in production are typically deliberate |
| **Effort** | 2 hours |

**Recommendation:** On SPI permission error during refresh, emit a clear error
message suggesting privilege check. Optionally track `GRANT`/`REVOKE` events.

---

### G3.5 — CREATE TRIGGER on Change Buffer Tables Not Blocked

| Field | Value |
|-------|-------|
| **Location** | `hooks.rs` |
| **Problem** | A user trigger on `pgtrickle_changes.changes_<oid>` could corrupt CDC data. The event trigger warns about triggers on source tables but doesn't protect change buffer tables. |
| **Severity** | **P3** |
| **Impact** | Very low — creating triggers on internal tables requires deliberate intent |
| **Effort** | 1 hour |

**Recommendation:** Extend the event trigger to `ERROR` on trigger creation
directed at tables in the `pgtrickle_changes` schema.

---

### G3.6 — Generated Column Exclusion Mismatch in schema_fingerprint

| Field | Value |
|-------|-------|
| **Location** | `cdc.rs:resolve_source_column_defs()` vs `catalog.rs` column snapshot |
| **Problem** | Generated columns (stored or virtual) are correctly excluded from CDC triggers (`attgenerated != ''`), but the column snapshot comparison may not apply the same filter. Adding a generated column could trigger unnecessary reinitialize. |
| **Severity** | **P4** |
| **Impact** | Very low — causes over-reinitialize, not incorrect data |
| **Effort** | 1–2 hours |

**Recommendation:** Align the column snapshot filter with the CDC column filter
to exclude `attgenerated != ''` columns.

---

## 7. Gap Category 4: Refresh Engine Edge Cases

### G4.1 — DELETE+INSERT Strategy: Remove

| Field | Value |
|-------|-------|
| **Location** | `refresh.rs`, `config.rs` — DELETE+INSERT merge strategy |
| **Problem** | The `delete_insert` strategy has three compounding issues: (1) double-evaluation against mutated state causes silent wrong results for aggregate/DISTINCT queries; (2) it is slower than `MERGE` for small deltas; (3) it is incompatible with prepared statements. The `auto` strategy already switches to bulk apply for large deltas — the only scenario where DELETE+INSERT has any advantage. |
| **Severity** | **P0 — Remove before public release** |
| **Decision** | Remove `delete_insert` as a valid `pg_trickle.merge_strategy` value. Accept only `auto` and `merge`. Emit `ERROR` if `delete_insert` is set. |
| **Effort** | 1–2 hours |

---

### G4.2 — LIMIT in Subquery Without ORDER BY

| Field | Value |
|-------|-------|
| **Location** | `parser.rs` — subquery and lateral subquery parsing |
| **Problem** | Top-level `LIMIT` is correctly rejected. However, `LIMIT` inside a FROM subquery or lateral subquery (e.g., `LATERAL (SELECT ... FROM t LIMIT 1)`) is **not** rejected and produces non-deterministic results if no `ORDER BY` is present. The full refresh and differential refresh could pick different rows. |
| **Severity** | **P2 — Non-deterministic results** |
| **Impact** | Medium — `LIMIT` in lateral subqueries is a common pattern |
| **Effort** | 3–4 hours |

**Options:**

**A. Warn (not reject) when `LIMIT` appears in a subquery without `ORDER BY`**
- ✅ Does not break existing queries that are deterministic in practice
- ✅ `LIMIT` with `ORDER BY` is a legitimate, safe pattern (e.g., `LATERAL (SELECT ... ORDER BY price LIMIT 1)`)
- ❌ Warning may be ignored; user can end up with silently non-deterministic results

**B. Reject `LIMIT` in subqueries entirely**
- ✅ Eliminates the non-determinism risk completely
- ❌ Breaks the common `LATERAL (SELECT ... ORDER BY ... LIMIT 1)` pattern, which is deterministic and useful
- ❌ Overly restrictive — the problem only exists without `ORDER BY`

**Decision:** Option A — warn when `LIMIT` appears without `ORDER BY`, allow when `ORDER BY` is present. The warning clearly communicates the risk without blocking legitimate deterministic uses.

---

### G4.3 — Adaptive FULL Fallback Threshold Not Observable

| Field | Value |
|-------|-------|
| **Location** | `refresh.rs` adaptive threshold auto-tuning |
| **Problem** | The auto-tuning clamps `auto_threshold` between 0.01 and 0.80 and adjusts based on workload patterns. No telemetry surfaces the current threshold or auto-tuning decisions. Operators cannot debug why a ST switches between DIFF and FULL. |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

**Recommendation:** Log threshold adjustments at DEBUG level. Add a
`pgtrickle.st_auto_threshold(st_name)` function or column to the monitoring view.

---

### G4.4 — Prepared Statement Plan Invalidation on DDL

| Field | Value |
|-------|-------|
| **Location** | `refresh.rs` prepared statements, DDL hooks |
| **Problem** | When `pg_trickle.use_prepared_statements = true`, the extension creates `PREPARE __pgt_merge_{id}`. On DDL events (e.g., adding an index to the stream table), the delta template cache is invalidated, but the PostgreSQL-side prepared statement plan is not `DEALLOCATE`-d. The cached plan may become suboptimal. |
| **Severity** | **P3** |
| **Effort** | 1–2 hours |

**Recommendation:** On DDL-triggered cache invalidation, also execute
`DEALLOCATE __pgt_merge_{id}` via SPI.

---

### G4.5 — User-Trigger Temp Table Leak on Error

| Field | Value |
|-------|-------|
| **Location** | `refresh.rs` — user-trigger explicit DML path |
| **Problem** | The explicit DML path materializes the delta to a temporary table, then runs DELETE/UPDATE/INSERT. If the INSERT step fails (e.g., constraint violation, OOM), the temp table may leak in the session's temp schema until session end. |
| **Severity** | **P3** |
| **Effort** | 1–2 hours |

**Recommendation:** Add temp table cleanup in the error path, or use
`ON COMMIT DROP` on the temp table creation.

---

### G4.6 — Wide Table MERGE IS DISTINCT FROM Chain

| Field | Value |
|-------|-------|
| **Location** | `refresh.rs` MERGE template generation |
| **Problem** | The MERGE UPDATE arm uses `IS DISTINCT FROM` per column for change detection. For very wide tables (100+ columns), this generates a long chain that causes plan cache bloat and slow planning. |
| **Severity** | **P4 — Performance only** |
| **Impact** | Only affects wide tables |
| **Effort** | 4–6 hours |

**Recommendation:** For tables exceeding a column-count threshold (e.g., 50),
use a hash-comparison shortcut: compare a single hash of all column values
instead of N individual `IS DISTINCT FROM` comparisons.

---

## 8. Gap Category 5: SQL Syntax & Expression Gaps

### G5.1 — DISTINCT ON Without ORDER BY Warning

| Field | Value |
|-------|-------|
| **Location** | `parser.rs:rewrite_distinct_on()` |
| **Problem** | `DISTINCT ON` is auto-rewritten to `ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...) = 1`. Without an `ORDER BY`, PostgreSQL picks an arbitrary row. The arbitrary choice may differ between full refresh and differential refresh (different execution plans), causing the stream table to non-deterministically switch rows. |
| **Severity** | **P3** |
| **Impact** | Low — `DISTINCT ON` without `ORDER BY` is bad practice |
| **Effort** | 1 hour |

**Recommendation:** Emit a WARNING when `DISTINCT ON` is used without `ORDER BY`.

---

### G5.2 — CUBE Combinatorial Explosion

| Field | Value |
|-------|-------|
| **Location** | `parser.rs:rewrite_grouping_sets()` |
| **Problem** | `CUBE(a, b, c, ..., n)` generates $2^n$ grouping sets, each becoming a UNION ALL branch. `CUBE` on 10 columns produces 1,024 branches; on 15 columns, 32,768 branches. The rewrite produces a massive UNION ALL tree that may exhaust memory during parsing or generate a delta query too large for PostgreSQL's parser. |
| **Severity** | **P2 — Potential OOM/crash** |
| **Impact** | Low — large CUBEs are uncommon |
| **Effort** | 1 hour |

**Recommendation:** Reject `CUBE`/`ROLLUP` that would generate more than a
configurable limit of branches (e.g., 64 or 128). Emit a clear error suggesting
explicit GROUPING SETS.

---

### G5.3 — T_XmlExpr Remains Unhandled

| Field | Value |
|-------|-------|
| **PostgreSQL syntax** | `XMLELEMENT`, `XMLFOREST`, `XMLPARSE`, `XMLPI`, `XMLROOT`, `XMLSERIALIZE`, `XMLCONCAT` |
| **Current behavior** | Rejected: `"Expression type T_XmlExpr is not supported"` |
| **Severity** | **P4** — extremely niche |
| **Recommendation** | Keep rejection. XML processing in PostgreSQL is rare. |

---

### G5.4 — T_GroupingFunc Outside Auto-Rewrite

| Field | Value |
|-------|-------|
| **PostgreSQL syntax** | `GROUPING(col1, col2)` in SELECT without GROUPING SETS |
| **Current behavior** | Rejected when encountered outside the GROUPING SETS rewrite path |
| **Severity** | **P4** — meaningless without GROUPING SETS |
| **Recommendation** | Keep rejection. |

---

### G5.5 — NATURAL JOIN Column Drift Not Tracked in Dependency Snapshot

| Field | Value |
|-------|-------|
| **Location** | `parser.rs` NATURAL JOIN handling |
| **Problem** | NATURAL JOIN is resolved by looking up common columns at parse time. If both tables later get a new column with the same name, the NATURAL JOIN's semantics change. The current `columns_used` tracking does not specifically track which columns were resolved for the NATURAL JOIN condition. |
| **Severity** | **P3** |
| **Impact** | Very low — requires adding identically-named columns to both source tables |
| **Effort** | 2–3 hours |

**Recommendation:** Store the resolved NATURAL JOIN column names in
`pgt_dependencies.columns_used` or a separate field. On schema change detection,
re-resolve and compare.

---

### G5.6 — RANGE_AGG / RANGE_INTERSECT_AGG Still Unrecognized

| Field | Value |
|-------|-------|
| **PostgreSQL syntax** | `RANGE_AGG(col)`, `RANGE_INTERSECT_AGG(col)` |
| **Current behavior** | Treated as scalar function → wrong results in GROUP BY |
| **Severity** | **P2** — misclassified as scalar |
| **Impact** | Very low — range aggregation is extremely niche |
| **Effort** | 1 hour — add to `is_known_aggregate()` and reject with clear message |

**Recommendation:** Add `range_agg` and `range_intersect_agg` to the aggregate
recognition list. Reject for DIFFERENTIAL mode (group-rescan) or implement.

---

## 9. Gap Category 6: Test Coverage Gaps

These are not code bugs but significant holes in the E2E test suite that
leave implemented features unverified under real PostgreSQL execution.

### G6.1 — 21 AggFunc Variants Have Zero E2E Differential Tests

| Untested Variant | Category |
|-----------------|----------|
| BOOL_AND, BOOL_OR | Boolean aggregates |
| ARRAY_AGG | Array aggregate |
| JSON_AGG, JSONB_AGG | JSON aggregates (legacy) |
| MODE | Ordered-set aggregate |
| PERCENTILE_DISC | Ordered-set aggregate |
| ANY_VALUE | PG 16+ aggregate |
| CORR, COVAR_POP, COVAR_SAMP | Regression statistics |
| REGR_AVGX, REGR_AVGY, REGR_COUNT, REGR_INTERCEPT, REGR_R2, REGR_SLOPE, REGR_SXX, REGR_SXY, REGR_SYY | Regression (12) |

All of these use the group-rescan strategy and have unit tests, but no E2E
test verifies correct delta computation under INSERT/UPDATE/DELETE cycles.

| **Severity** | **P3** |
|-------|-------|
| **Effort** | 6–8 hours (batch create ~20 tests) |

**Recommendation:** Create a batch E2E test file `e2e_aggregate_coverage_tests.rs`
with one test per untested aggregate, exercising the full cycle:
`INSERT → create_st → refresh → assert → DML → refresh → assert_matches_query`.

---

### G6.2 — FULL JOIN Has Zero E2E Tests

| Field | Value |
|-------|-------|
| **OpTree** | `FullJoin` |
| **Operator** | `diff_full_join` (422 lines of delta SQL generation) |
| **Current E2E** | None |
| **Severity** | **P3** |
| **Effort** | 3–4 hours |

**Recommendation:** Create E2E tests for: (a) basic FULL JOIN creation, (b)
INSERT on left-only side, (c) INSERT on right-only side, (d) INSERT matching
both sides, (e) DELETE from one side, (f) UPDATE on join key.

---

### G6.3 — INTERSECT / EXCEPT Have Zero E2E Tests

| Field | Value |
|-------|-------|
| **OpTree** | `Intersect`, `Except` |
| **Operators** | `diff_intersect` (400 lines), `diff_except` (436 lines) |
| **Current E2E** | None |
| **Severity** | **P3** |
| **Effort** | 3–4 hours |

**Recommendation:** Test both `INTERSECT` and `INTERSECT ALL`, `EXCEPT` and
`EXCEPT ALL`, with DML on both branches.

---

### G6.4 — ScalarSubquery in SELECT Has Zero E2E Tests

| Field | Value |
|-------|-------|
| **OpTree** | `ScalarSubquery` |
| **Operator** | `diff_scalar_subquery` (233 lines) |
| **Scenario** | `SELECT (SELECT MAX(x) FROM ref), a, b FROM main` — change to `ref` table should update all rows |
| **Current E2E** | None |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

---

### G6.5 — SubLinks in OR Rewrite Has Zero E2E Tests

| Field | Value |
|-------|-------|
| **Auto-rewrite** | `rewrite_sublinks_in_or()` |
| **Scenario** | `WHERE x > 5 OR EXISTS (SELECT 1 FROM vip WHERE vip.id = t.id)` |
| **Current E2E** | None |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

---

### G6.6 — Multi-Partition Window Rewrite Has Zero E2E Tests

| Field | Value |
|-------|-------|
| **Auto-rewrite** | `rewrite_multi_partition_windows()` |
| **Scenario** | `SELECT ROW_NUMBER() OVER (PARTITION BY a), SUM(x) OVER (PARTITION BY b) FROM t` |
| **Current E2E** | None |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

---

### G6.7 — GUC Variation Tests Missing Entirely

No E2E test exercises non-default GUC settings:

| GUC | Default | Untested Setting |
|-----|---------|-----------------|
| `pg_trickle.block_source_ddl` | `false` | `true` — should block `ALTER TABLE` |
| `pg_trickle.merge_strategy` | `'auto'` | `'delete_insert'` |
| `pg_trickle.use_prepared_statements` | `false` | `true` |
| `pg_trickle.merge_planner_hints` | `true` | `false` |
| `pg_trickle.cleanup_use_truncate` | `false` | `true` |

| **Severity** | **P3** |
|-------|-------|
| **Effort** | 4–6 hours |

---

### G6.8 — Multi-Cycle Refresh Tests Rare

Most E2E tests do one DML → one refresh. The delta template cache, prepared
statement cache, deferred cleanup, and adaptive threshold logic only activate
on cycle 2+. Almost no tests verify multi-cycle correctness.

| **Severity** | **P3** |
|-------|-------|
| **Effort** | 3–4 hours |

**Recommendation:** Add tests with DML → refresh → DML → refresh → assert for
the most important operators (aggregate, join, window).

---

## 10. Gap Category 7: CDC & Change Buffer Issues

### G7.1 — Keyless Table Content Hash Produces Identical Row IDs for Duplicates

| Field | Value |
|-------|-------|
| **Location** | `cdc.rs` trigger function, `hash.rs` |
| **Problem** | For tables without a PRIMARY KEY, `pk_hash` = `pg_trickle_hash(row_to_json(NEW)::text)`. Two rows with identical content produce the same hash. The MERGE's `WHEN MATCHED` arm sees them as one row. Inserting a duplicate looks like no change; deleting one of two identical rows may delete both. |
| **Severity** | **P1 — Incorrect delta for tables with duplicate rows and no PK** |
| **Impact** | Low — requires keyless tables + exact duplicates |
| **Effort** | 4–6 hours |

**Options:**

**A. Document the limitation**
- ✅ Zero code change
- ✅ The failure mode requires an unusual combination: keyless table + exact duplicate rows
- ❌ Silent wrong results for anyone who hits this edge case

**B. Use `ctid` as a tiebreaker**
- ✅ `ctid` is always unique within a table at a point in time
- ❌ `ctid` changes after VACUUM, CLUSTER, or UPDATE (which rewrites the row) — the tiebreaker itself becomes unstable
- ❌ Using `ctid` in the change buffer means the delta can't match rows across a VACUUM, causing ghost deletes

**C. Require UNIQUE constraint for DIFFERENTIAL mode on keyless tables**
- ✅ Clean user-facing contract: DIFFERENTIAL requires a PK or UNIQUE constraint
- ✅ Consistent with the G2.1 decision (requiring PK for WAL mode)
- ❌ Existing keyless-table stream tables in DIFFERENTIAL mode would break on upgrade

**Decision:** Option A — document prominently. This limitation was already covered by F11 in v0.1.0. The `ctid` approach is too fragile, and requiring a unique constraint for DIFFERENTIAL is a breaking change. The combination of keyless + exact duplicates is sufficiently rare that documentation is the right call.

---

### G7.2 — Change Buffer Column Accumulation on ALTER TABLE DROP COLUMN

| Field | Value |
|-------|-------|
| **Location** | `cdc.rs:sync_change_buffer_columns()` |
| **Problem** | After `ALTER TABLE ... ADD COLUMN`, the buffer table gets the new column. After `ALTER TABLE ... DROP COLUMN`, the old `new_*`/`old_*` columns are never removed. Buffer tables accumulate dead columns over time. |
| **Severity** | **P3 — Wasted storage, no correctness impact** |
| **Impact** | Low — delta queries reference columns by name |
| **Effort** | 2–3 hours |

---

### G7.3 — Change Buffer Index Covering Overhead

| Field | Value |
|-------|-------|
| **Location** | `cdc.rs` — index on `(lsn, pk_hash, change_id) INCLUDE (action)` |
| **Problem** | The INCLUDE clause adds the `action` column for index-only scans. On high-write tables, the index maintenance overhead may exceed the benefit. |
| **Severity** | **P4 — Performance tuning** |
| **Effort** | Benchmark only |

---

### G7.4 — Change Buffer Schema Public Access

| Field | Value |
|-------|-------|
| **Location** | `config.rs:PGS_CHANGE_BUFFER_SCHEMA`, `cdc.rs` |
| **Problem** | The `pgtrickle_changes` schema is created with default permissions. Any user with database access can INSERT/UPDATE/DELETE rows in change buffer tables, injecting bogus changes that get applied on next refresh. |
| **Severity** | **P4 — Security concern in shared-database environments** |
| **Impact** | Production systems with shared access |
| **Effort** | 1–2 hours |

**Recommendation:** `REVOKE ALL ON SCHEMA pgtrickle_changes FROM PUBLIC` during
extension creation. Grant access only to the extension owner.

---

## 11. Gap Category 8: Production & Deployment Concerns

### G8.1 — PgBouncer Transaction-Mode Incompatibility

| Field | Value |
|-------|-------|
| **Problem** | PgBouncer in transaction-mode pooling does not support session-level prepared statements (`PREPARE`/`EXECUTE`), session-level advisory locks, or `LISTEN`/`NOTIFY`. The scheduler uses advisory locks for concurrency control; the refresh engine optionally uses prepared statements; NOTIFY is used for alerts. |
| **Impact** | Advisory locks may be released on connection return to pool → concurrent refreshes. Prepared statements → "does not exist" errors. NOTIFY → lost alerts. |
| **Severity** | **P1** |
| **Effort** | 4–6 hours |

**Options:**

**A. Document the incompatibility — require session-mode pooling or a direct connection**
- ✅ Zero code change — no implementation risk or maintenance burden
- ✅ Standard practice: many PostgreSQL extensions document the same restriction (PostGIS, pgcrypto, etc.)
- ✅ Session-mode pooling and direct connections are common in self-hosted and CNPG deployments
- ❌ Transaction-mode pooling is the default at many cloud providers (RDS Proxy, Supabase, Neon) — permanently blocks those users

**B. Replace advisory locks with transaction-scoped locking + eliminate prepared statements**
- ✅ Makes transaction-mode pooling work — `pg_advisory_xact_lock()` and `FOR UPDATE SKIP LOCKED` are transaction-scoped
- ✅ Removes all session-state dependencies — fully cloud-native
- ❌ Large refactor: advisory locks are used in refresh, CDC, and scheduling coordination throughout the codebase
- ❌ Must also eliminate or rework prepared statements (`PREPARE __pgt_merge_*`) which are also session-scoped
- ❌ Scope is v0.3.0+ — not appropriate as a v0.2.0 correctness fix

**Decision:** Option A for v0.2.0 — document clearly. Option B is architecturally desirable but must be paired with eliminating prepared statements; it belongs in v0.3.0 operational hardening.

---

### G8.2 — Read Replica Behavior

| Field | Value |
|-------|-------|
| **Problem** | On a read replica, CDC triggers are replayed from WAL but don't fire independently. The background worker starts but cannot write. Manual `refresh_stream_table()` fails with a read-only error. |
| **Severity** | **P2** |
| **Effort** | 2–3 hours |

**Options:**

**A. Detect replica mode at startup and skip background worker registration**
- ✅ Extension loads silently on replicas — no errors, no background worker slot consumed
- ✅ The replica stays consistent via the primary's WAL replay — no refresh needed
- ✅ `pg_is_in_recovery()` is a simple, reliable detection mechanism
- ✅ Promotion is detected automatically: `pg_is_in_recovery()` changes to `false`

**B. Allow worker to start on replicas, fail loudly on first write attempt**
- ✅ Slightly simpler: no upfront detection code
- ❌ Consumes a background worker slot and a connection before failing
- ❌ Error message from a failed SPI write attempt is cryptic and confusing for users

**Decision:** Option A — detect replica mode at startup and skip worker registration. Emit a clear error message for manual `refresh_stream_table()` calls on replicas.

---

### G8.3 — Extension Upgrade Path

| Field | Value |
|-------|-------|
| **Problem** | No `ALTER EXTENSION pg_trickle UPDATE` migration SQL files exist. Upgrading the extension binary without a migration path strands the catalog at the old schema version. |
| **Severity** | **P3** |
| **Effort** | See `plans/sql/PLAN_DB_SCHEMA_STABILITY.md` for full analysis |

**Recommendation:** Implement versioned SQL migration files before 1.0 release.

---

### G8.4 — Memory Bounds on Delta Materialization

| Field | Value |
|-------|-------|
| **Problem** | The delta query is a single SQL statement. For large batches (e.g., bulk UPDATE of 10M rows), PostgreSQL may attempt to materialize the entire delta, exceeding `work_mem` and potentially OOM-killing the backend. |
| **Severity** | **P3** |
| **Effort** | 8–12 hours (chunked delta processing) |

**Recommendation:** Short-term: document the risk and recommend the GUC
`pg_trickle.differential_max_change_ratio` to trigger FULL fallback for large
batches. Long-term: implement batched delta processing.

---

### G8.5 — Sequential ST Processing Despite Concurrency GUC

| Field | Value |
|-------|-------|
| **Location** | `scheduler.rs` main loop |
| **Problem** | `pg_trickle.max_concurrent_refreshes` is configurable up to 32, but the scheduler processes STs sequentially in topological order within a single background worker. Parallel refresh is not implemented. |
| **Severity** | **P3** |
| **Effort** | 12–16 hours (parallel refresh implementation) |

**Recommendation:** Document that parallel refresh is not yet operational.
Consider spawning additional background workers for independent DAG branches.

---

### G8.6 — SPI Error Retry Classification Too Broad

| Field | Value |
|-------|-------|
| **Location** | `error.rs` — `SpiError` variants |
| **Problem** | All SPI errors are classified as `System` (retryable). This includes permission errors (42xxx), constraint violations (23xxx), and division-by-zero — none retryable. The scheduler wastes retry attempts on non-transient errors. |
| **Severity** | **P3** |
| **Effort** | 3–4 hours |

**Recommendation:** Parse SQLSTATE error codes and classify: permission errors →
non-retryable User error; serialization failures (40001) → retryable System;
constraint violations → non-retryable.

---

### G8.7 — Connection/Worker Overhead Documentation

| Field | Value |
|-------|-------|
| **Problem** | Each database with pg_trickle enabled consumes at least 1 background worker connection. WAL decoder uses additional connections. No documentation quantifies this overhead. |
| **Severity** | **P4** |
| **Effort** | 1 hour (documentation) |

---

## 12. Gap Category 9: Monitoring & Observability

### G9.1 — No Delta Size or Row Count Metrics

| Field | Value |
|-------|-------|
| **Problem** | `pgt_refresh_history` records total execution time but not the number of rows processed in the delta, the merge strategy used, or whether a FULL fallback occurred. Operators cannot distinguish between a fast no-op refresh and a fast small-delta refresh. |
| **Severity** | **P3** |
| **Effort** | 3–4 hours |

**Recommendation:** Add `delta_row_count`, `merge_strategy_used`, and
`was_full_fallback` columns to `pgt_refresh_history`.

---

### G9.2 — No Memory/Temp File Usage Tracking

| Field | Value |
|-------|-------|
| **Problem** | No visibility into delta query memory consumption. Large deltas spill to temp files with no advance warning or after-the-fact metric. |
| **Severity** | **P3** |
| **Effort** | 4–6 hours |

**Recommendation:** After each refresh, query `pg_stat_statements` (if available)
for the delta query's `temp_blks_written` metric. Expose via monitoring view.

---

### G9.3 — Buffer Growth Alert Threshold Not Configurable

| Field | Value |
|-------|-------|
| **Location** | `monitor.rs:check_slot_health_and_alert()` — `pending > 1_000_000` |
| **Problem** | Hardcoded 1M row threshold. High-throughput workloads may routinely exceed this; small tables may need a lower threshold. |
| **Severity** | **P4** |
| **Effort** | 1 hour |

**Recommendation:** Make configurable via a GUC (e.g.,
`pg_trickle.buffer_alert_threshold`).

---

### G9.4 — No NOTIFY for Schedule Misses

| Field | Value |
|-------|-------|
| **Problem** | If a ST's refresh consistently takes longer than its schedule interval (stale data condition), no NOTIFY alert is sent. The `StaleData` alert exists but may not fire in all scheduler code paths. |
| **Severity** | **P3** |
| **Effort** | 2–3 hours |

**Recommendation:** Ensure the scheduler emits `StaleData` alerts whenever
`last_refresh_time + schedule < now()`.

---

### G9.5 — Adaptive Threshold Not Exposed

| Field | Value |
|-------|-------|
| **Problem** | The auto-tuned adaptive threshold (for DIFF→FULL fallback) is stored in shared memory but not exposed to SQL. Operators cannot see why a ST switches strategies. |
| **Severity** | **P3** |
| **Effort** | 1–2 hours |

**Recommendation:** Add to `stream_tables_info` view or expose via
`pgtrickle.st_auto_threshold(name)` function.

---

## 13. Gap Category 10: Intentional Rejections (Confirmed OK)

These constructs are permanently rejected by design. Reviewed and confirmed
correct in this analysis:

| Construct | Reason | Status |
|-----------|--------|--------|
| LIMIT / OFFSET / FETCH FIRST | Stream tables are full result sets | ✅ Correct |
| FOR UPDATE / FOR SHARE / FOR NO KEY UPDATE | Row-level locking meaningless | ✅ Correct |
| TABLESAMPLE | Non-deterministic sampling | ✅ Correct |
| ROWS FROM() with multiple functions | Extremely niche | ✅ Correct |
| LATERAL with RIGHT/FULL JOIN | PostgreSQL itself limits this | ✅ Correct |
| XMLAGG | Extremely niche | ✅ Correct |
| Hypothetical-set aggregates | Better as window functions | ✅ Correct |
| Window functions in expressions | Architectural constraint | ✅ Correct |
| ORDER BY | Accepted but no-op (row order undefined) | ✅ Correct |
| Direct DML on stream tables | Refresh engine manages writes | ✅ Correct |
| Direct DDL on storage tables | DDL hooks warn/block | ✅ Correct |
| SELECT without FROM | No CDC source | ✅ Correct |
| User-defined aggregates in DIFF | Unknown delta rules | ✅ Correct |
| Materialized views in DIFF | No CDC mechanism | ✅ Correct |
| Foreign tables in DIFF | No CDC mechanism | ✅ Correct |

---

## 14. Prioritized Implementation Roadmap

### Tier 0 — Critical Correctness (Must Fix Before 1.0)

| Step | Gap | Description | Effort | Priority | Status |
|------|-----|-------------|--------|----------|--------|
| **F1** | G4.1 | Remove `delete_insert` strategy | 1–2h | P0 | ✅ Done (code removed in v0.2.0; stale ARCHITECTURE.md row cleaned up) |
| **F2** | G2.1 | WAL decoder: keyless table pk_hash | 4–6h | P1 | ✅ Done (guard rejects keyless tables + requires REPLICA IDENTITY FULL) |
| **F3** | G2.2 | WAL decoder: old_* columns for UPDATE | 8–12h | P1 | ✅ Done (parse_pgoutput_old_columns + old-key→new-tuple section parsing) |
| **F4** | G2.3 | WAL decoder: pgoutput action parsing | 2–3h | P1 | ✅ Already done (positional parsing, not string search) |
| **F5** | G1.1 | JOIN key change + right-side delete | Document | P1 | ✅ Already done (documented in SQL_REFERENCE.md) |
| **F6** | G3.1 | Track ALTER TYPE events | 3–4h | P1 | ✅ Done (handle_type_change + find_sts_using_type in hooks.rs) |
| **F7** | G3.3 | Track ALTER POLICY / RLS events | 3–4h | P1 | ✅ Done (handle_policy_change in hooks.rs) |

**Estimated effort:** 22–33 hours → **Actual: ~8 hours (3 items were already done)**  
**Value:** Closes all P0 and P1 items. WAL decoder fixes (F2-F4) are prerequisite
for promoting WAL CDC to production.  
**Status: ✅ COMPLETE**

### Tier 1 — High-Value Correctness Verification

| Step | Gap | Description | Effort | Priority | Status |
|------|-----|-------------|--------|----------|--------|
| **F8** | G1.2 | Window partition key change: E2E test | 4–6h | P1 | ✅ Done (2 E2E tests in e2e_window_tests.rs) |
| **F9** | G1.3 | Recursive CTE monotonicity audit | 6–8h | P1 | ⬜ Not started (audit-gated) |
| **F10** | G3.2 | Track ALTER DOMAIN events | 2–3h | P1 | ✅ Done (handle_domain_change in hooks.rs) |
| **F11** | G7.1 | Keyless table duplicate rows: document | 1h | P1 | ✅ Done (SQL_REFERENCE.md expanded with G7.1 warning) |
| **F12** | G8.1 | PgBouncer: document + fix advisory locks | 4–6h | P1 | ✅ Done (FAQ.md PgBouncer compatibility section) |

**Estimated effort:** 17–24 hours → **Actual: ~4 hours (F9 deferred for audit)**  
**Value:** Resolves remaining P1 items. F8 and F9 may be downgraded to P4 after
verification tests pass.  
**Status: 4/5 COMPLETE (F9 pending audit)**

### Tier 2 — Robustness & P2 Fixes

| Step | Gap | Description | Effort | Priority |
|------|-----|-------------|--------|----------|
| **F13** | G4.2 | Warn on LIMIT in subquery without ORDER BY | 3–4h | P2 |
| **F14** | G5.2 | CUBE combinatorial explosion: reject large CUBEs | 1h | P2 |
| **F15** | G5.6 | RANGE_AGG/RANGE_INTERSECT_AGG recognition | 1h | P2 |
| **F16** | G8.2 | Detect read replicas, skip worker | 2–3h | P2 |

**Estimated effort:** 7–9 hours  
**Value:** Prevents crashes and confusing errors in edge cases.

### Tier 3 — Test Coverage (No New Features, High ROI)

| Step | Gap | Description | Effort |
|------|-----|-------------|--------|
| **F17** | G6.1 | 21 aggregate E2E differential tests | 6–8h |
| **F18** | G6.2 | FULL JOIN E2E tests | 3–4h |
| **F19** | G6.3 | INTERSECT/EXCEPT E2E tests | 3–4h |
| **F20** | G6.4 | ScalarSubquery E2E tests | 2–3h |
| **F21** | G6.5 | SubLinks-in-OR E2E tests | 2–3h |
| **F22** | G6.6 | Multi-partition window E2E tests | 2–3h |
| **F23** | G6.7 | GUC variation E2E tests | 4–6h |
| **F24** | G6.8 | Multi-cycle refresh E2E tests | 3–4h |
| **F25** | G1.4 | HAVING group transition E2E test | 2–3h |
| **F26** | G1.6 | FULL JOIN NULL keys E2E test | 2h |

**Estimated effort:** 29–38 hours  
**Value:** Verifies existing code correctness without adding new features.
Catches regressions. May surface P1 bugs in untested operators.

### Tier 4 — Operational Hardening

| Step | Gap | Description | Effort |
|------|-----|-------------|--------|
| **F27** | G4.3 | Expose adaptive threshold | 2–3h |
| **F28** | G4.4 | DEALLOCATE prepared statements on DDL | 1–2h |
| **F29** | G8.6 | Parse SPI SQLSTATE for retry classification | 3–4h |
| **F30** | G9.1 | Add delta_row_count to refresh history | 3–4h |
| **F31** | G9.4 | Emit StaleData NOTIFY consistently | 2–3h |
| **F32** | G2.4 | WAL transition retry with backoff | 3–4h |
| **F33** | G2.5 | WAL column rename detection | 2–3h |
| **F34** | G3.4 | Clear error on SPI permission failure | 2h |
| **F35** | G3.5 | Block triggers on change buffer tables | 1h |
| **F36** | G4.5 | Temp table cleanup on error | 1–2h |
| **F37** | G5.1 | DISTINCT ON without ORDER BY warning | 1h |
| **F38** | G5.5 | NATURAL JOIN column drift tracking | 2–3h |
| **F39** | G7.2 | Drop orphaned buffer table columns | 2–3h |
| **F40** | G8.3 | Extension upgrade migration scripts | See PLAN_DB_SCHEMA_STABILITY.md |

**Estimated effort:** 25–36 hours  
**Value:** Production polish. Many items are 1–3 hour fixes.

### Tier 5 — Nice-to-Have

| Step | Gap | Description | Effort |
|------|-----|-------------|--------|
| **F41** | G4.6 | Wide table MERGE hash shortcut | 4–6h |
| **F42** | G8.4 | Document delta memory bounds | 1h |
| **F43** | G8.5 | Document sequential processing | 1h |
| **F44** | G8.7 | Document connection overhead | 1h |
| **F45** | G9.2 | Memory/temp file usage tracking | 4–6h |
| **F46** | G9.3 | Buffer alert threshold GUC | 1h |
| **F47** | G9.5 | Expose adaptive threshold function | 1–2h |
| **F48** | G1.5 | Keyless table duplicate rows E2E | 2–3h |
| **F49** | G3.6 | Generated column snapshot filter alignment | 1–2h |
| **F50** | G7.3 | Benchmark covering index overhead | 2h |
| **F51** | G7.4 | Change buffer schema permissions | 1–2h |

**Estimated effort:** 19–30 hours

### Summary

| Tier | Steps | Effort | Cumulative | Status |
|------|-------|--------|------------|--------|
| 0 — Critical | F1–F7 | 22–33h → ~8h | ~8h | ✅ Complete |
| 1 — Verification | F8–F12 | 17–24h → ~4h | ~12h | 4/5 (F9 pending) |
| 2 — Robustness | F13–F16 | 7–9h | ~21h | Not started |
| 3 — Test Coverage | F17–F26 | 29–38h | ~59h | Not started |
| 4 — Operational | F27–F40 | 25–36h | ~95h | Not started |
| 5 — Nice-to-Have | F41–F51 | 19–30h | ~125h | Not started |
| **Total** | **51 steps** | **~125h remaining** | — | **11/51 done** |

### Recommended Execution Order

```
Session 1:  F1 (remove delete_insert) + F4 (pgoutput parsing)        ~4h
Session 2:  F6 (ALTER TYPE) + F7 (ALTER POLICY) + F10 (ALTER DOMAIN) ~8h
Session 3:  F8 (window partition E2E) + F9 (recursive monotonicity)  ~12h
Session 4:  F13 (LIMIT warning) + F14 (CUBE limit) + F15 (RANGE_AGG) ~5h
Session 5:  F17–F22 (test coverage batch 1: aggregates, FULL JOIN)   ~18h
Session 6:  F23–F26 (test coverage batch 2: GUCs, multi-cycle)       ~11h
Session 7:  F2 (WAL pk_hash) + F3 (WAL old_*)                        ~14h
Session 8:  F11 (keyless docs) + F12 (PgBouncer) + F16 (replica)     ~7h
Session 9+: F27–F51 (operational hardening)                           ~44h
```

**Note:** F2, F3, and F7 are the most impactful WAL decoder fixes. Sessions 1–4
focus on non-WAL issues that affect the current default trigger-based CDC.
WAL decoder fixes are deferred to Session 7 since they only matter when WAL
mode is explicitly enabled.

---

## 15. Historical Progress Summary

| Plan | Phase | Sessions | Items Resolved | Test Growth |
|------|-------|----------|---------------|-------------|
| SQL_GAPS_1 | Expression deparsing, CROSS JOIN, FOR UPDATE rejection | 1 | 6 | 745 → 757 |
| SQL_GAPS_2 | GROUPING SETS P0, GROUP BY hardening, TABLESAMPLE | 1 | 6 | 745 → 750 |
| SQL_GAPS_3 | Aggregates (5 new), subquery operators (Semi/Anti/Scalar Join) | ~5 | 10 | 750 → 809 |
| SQL_GAPS_4 | Report accuracy, ordered-set aggregates (MODE, PERCENTILE) | 1 | 7 | 809 → 826 |
| Hybrid CDC | Trigger→WAL transition, user triggers, pgt_ rename | ~3 | 12+ | 826 → 872 |
| SQL_GAPS_5 | 15 steps: volatile detection, DISTINCT ON, GROUPING SETS, ALL subquery, regression aggs, mixed UNION, TRUNCATE, schema infra, NATURAL JOIN, keyless tables, scalar WHERE, SubLinks-OR, multi-PARTITION, recursive CTE DIFF | ~10 | 15 | 872 → ~920 |
| SQL_GAPS_6 | 38 gaps: views, JSON_TABLE, IS JSON, SQL/JSON constructors, COLLATE, virtual gen cols, foreign tables, partitioned CDC, replication detection, operator volatility, cache invalidation, function DDL, 4 doc tiers | ~8 | 23/24 (F15 deferred) | ~920 → ~1,150 |
| **SQL_GAPS_7** | **53 gaps: delta correctness, WAL decoder, DDL tracking, refresh engine, test coverage, CDC, production deployment, monitoring** | **Session 1** | **11/51 (Tier 0 ✅, Tier 1 4/5)** | **~923 unit, 386 E2E** |

### Cumulative Scorecard

| Metric | SQL_GAPS_1 | SQL_GAPS_3 | SQL_GAPS_5 | SQL_GAPS_6 | SQL_GAPS_7 (Now) |
|--------|-----------|-----------|------------|------------|-----------------|
| AggFunc variants | 5 | 10 | 36 | 39 | 39 |
| OpTree variants | 12 | 18 | 21 | 22 | 22 |
| Diff operators | 10 | 16 | 21 | 21 | 21 |
| Auto-rewrite passes | 0 | 0 | 5 | 5 | 6 |
| Unit tests | 745 | 809 | ~896 | ~920 | ~920 |
| E2E tests | ~100 | ~200 | ~350 | ~384 | 384 |
| E2E test files | ~15 | ~18 | 22 | 23 | 23 |
| P0 issues | 14 | 0 | 0 | 0 | 1 (gated) |
| P1 issues | 5 | 0 | 0 | 0 | 11 |
| Expression types | 7 | 15 | 30+ | 40+ | 40+ |
| GUCs | ~6 | ~8 | 17 | 17 | 20 |
| Total source lines | ~12K | ~18K | ~30K | ~35K | 40,094 |
| Parser lines | ~5K | ~7K | ~10K | ~11K | 13,108 |

### What Changed Between SQL_GAPS_6 and SQL_GAPS_7

SQL_GAPS_6 focused on **SQL syntax coverage** and **operational boundary
conditions** — does pg_trickle handle every PostgreSQL SELECT construct, and does
it interact correctly with views, foreign tables, partitioned tables, JSON_TABLE,
virtual generated columns, etc.? All P0 and P1 syntax items were resolved.

SQL_GAPS_7 shifts focus to **second-order correctness** — the edge cases that
emerge when real-world mutation patterns interact with the delta computation
engine:

- **Delta operator edge cases:** What happens when JOIN keys change simultaneously?
  When window partition keys are updated? When recursive CTEs are non-monotone?
  When HAVING groups transition across the predicate boundary?

- **WAL decoder correctness:** The trigger-based CDC is solid, but the WAL
  decoder has three P1 issues (pk_hash, old_*, parsing) that must be fixed
  before promoting WAL mode to production.

- **DDL tracking completeness:** ALTER TABLE is well-tracked, but ALTER TYPE,
  ALTER DOMAIN, and ALTER POLICY changes can silently change stream table
  semantics.

- **Test coverage:** 21 aggregate functions, FULL JOIN, INTERSECT/EXCEPT,
  ScalarSubquery, two auto-rewrite passes, and all GUC variations have zero
  E2E tests. These are implemented and unit-tested features that lack real
  PostgreSQL validation.

- **Production deployment:** PgBouncer incompatibility, read replica behavior,
  extension upgrade path, and memory bounds are undocumented or unhandled.

This represents the final maturation stage from "does the SQL parse?" and "do
the operators compute correct deltas?" to "does the system behave correctly
and robustly in all edge cases under production conditions?"

### Assessment: 1.0 Readiness

| Area | Status | Blockers |
|------|--------|----------|
| **SQL syntax coverage** | ✅ Complete | None — every SELECT construct handled or rejected |
| **Core delta operators** | ✅ Solid | F5 ✅ documented; F8 ✅ 2 E2E tests added |
| **Aggregate coverage** | ✅ Complete (39 functions) | F17: 21 variants lack differential E2E tests |
| **CDC (trigger-based)** | ✅ Production-ready | F11 ✅ keyless duplicate row limitation documented |
| **CDC (WAL-based)** | ⚠️ Improved | F2 ✅, F3 ✅, F4 ✅ — pk guard, old_* columns, positional parsing |
| **DDL tracking** | ✅ Complete | F6 ✅, F7 ✅, F10 ✅ — ALTER TYPE/DOMAIN/POLICY tracked |
| **Refresh engine** | ✅ Solid | F1 ✅ `delete_insert` removed in v0.2.0, stale docs cleaned |
| **Production deployment** | ⚠️ Improved | F12 ✅ PgBouncer documented; F16: replicas; F40: upgrades |
| **Test coverage** | ⚠️ Significant gaps | F17–F26: 10 test suite expansion tasks |
| **Monitoring** | ⚠️ Basic | F27–F31: observability improvements |

**Minimum viable 1.0:** Tiers 0 + 1 are now nearly complete (~12 hours actual).
The remaining F9 (recursive CTE monotonicity audit) is audit-gated.
Tier 3 (~29–38 hours) provides the E2E test confidence needed for a public
release. Total remaining for 1.0: ~35–46 hours across ~4–5 sessions.

### Prioritized Remaining Work

1. **F9** (Tier 1) — Recursive CTE monotonicity audit. Audit `src/dvm/operators/recursive_cte.rs`, add E2E tests for non-monotone queries, decide between fix and rejection. ~6–8h.
2. **F13–F16** (Tier 2) — LIMIT warning, CUBE rejection, RANGE_AGG, replica detection. ~7–9h.
3. **F17–F26** (Tier 3) — 10 E2E test suite expansion tasks covering aggregates, FULL JOIN, INTERSECT/EXCEPT, scalar subqueries, GUC variations, multi-cycle refresh, HAVING transitions. ~29–38h.
4. **F27–F40** (Tier 4) — Operational hardening: adaptive threshold, prepared statement cleanup, SPI retry classification, monitoring, WAL transition backoff, etc. ~25–36h.
5. **F41–F51** (Tier 5) — Nice-to-have: wide table MERGE optimization, documentation, memory tracking, buffer alerts, permissions. ~19–30h.
