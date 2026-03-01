# PLAN: SQL Gaps — Phase 4

**Status:** Complete  
**Date:** 2026-02-22
**Branch:** `main`
**Scope:** Full evaluation of SQL gap status, GAP_SQL_OVERVIEW.md accuracy audit, and prioritized remaining work.
**Current state:** 826 unit tests, 21 E2E test files, 25 AggFunc variants, 21 OpTree variants, 20 diff operators

---

## Evaluation of Previous Plans

### PLAN_SQL_GAPS_1.md — ✅ COMPLETE (6/6 tasks)

| Task | Description | Status |
|------|-------------|--------|
| 1 | Window-in-subexpression recursive detection (14 node types) | ✅ |
| 2 | GAP_SQL_OVERVIEW.md discrepancy cleanup | ✅ |
| 3 | Stale comment fix on `check_from_item_unsupported()` | ✅ |
| 4 | CROSS JOIN test coverage (3 unit + 4 E2E) | ✅ |
| 5 | FOR UPDATE/FOR SHARE rejection (4 E2E) | ✅ |
| 6 | Documentation updates (README, SQL_REFERENCE) | ✅ |

No remaining work.

### PLAN_SQL_GAPS_2.md — ✅ COMPLETE (6/6 tasks)

| Task | Description | Status |
|------|-------------|--------|
| 1 | GROUPING SETS silent-skip P0 bug fix | ✅ |
| 2 | GROUP BY `if let Ok` → `?` error propagation hardening | ✅ |
| 3 | TABLESAMPLE explicit rejection | ✅ |
| 4 | GAP_SQL_OVERVIEW.md Gap 6.2 correction | ✅ |
| 5 | Aggregate rejection E2E test coverage | ✅ |
| 6 | Documentation updates | ✅ |

No remaining work.

### PLAN_SQL_GAPS_3.md — ✅ COMPLETE (10/10 tasks)

| Task | Description | Status |
|------|-------------|--------|
| 1 | BIT_AND/OR/XOR AggFunc variants | ✅ |
| 2 | JSON_OBJECT_AGG / JSONB_OBJECT_AGG | ✅ |
| 3 | Documentation for Session 1 | ✅ |
| 4 | Statistical aggregates (STDDEV_POP/SAMP, VAR_POP/SAMP) | ✅ |
| 5 | Documentation for Session 2 | ✅ |
| 6 | EXISTS subquery — OpTree + parser (SemiJoin, AntiJoin) | ✅ |
| 7 | EXISTS subquery — delta operators (semi_join.rs, anti_join.rs) | ✅ |
| 8 | IN (SELECT ...) subquery parsing | ✅ |
| 9 | Scalar subquery (ScalarSubquery OpTree + operator) | ✅ |
| 10 | Documentation for Sessions 3–5 | ✅ |

Sessions 6–8 (Tier 3 structural enhancements) were listed as deferred. These are the remaining work.

---

## GAP_SQL_OVERVIEW.md — Accuracy Audit

### 🔴 Critical Inaccuracies Found

The report has **not been updated** to reflect PLAN_SQL_GAPS_3 completion. Multiple sections are stale:

| Section | Report Claims | Actual State | Severity |
|---------|--------------|--------------|----------|
| **Last Updated (L6)** | "PLAN_SQL_GAPS_2 Tasks 1-3 complete" | PLAN_SQL_GAPS_3 Tasks 1-10 also complete | 🔴 Stale |
| **Executive summary (L31-35)** | "Remaining gaps include: correlated subquery expressions (EXISTS, IN, scalar subqueries)" | EXISTS, IN, NOT EXISTS, NOT IN, scalar subquery are ALL implemented | 🔴 Wrong |
| **Test count (L37)** | "783 unit tests passing" | 809 unit tests passing | 🟡 Outdated |
| **Gap 4 intro (L403-405)** | "No subquery expression types produce valid SQL" | EXISTS, IN, scalar subquery all produce valid delta SQL | 🔴 Wrong |
| **Gap 4.1 (L407)** | "✅ FIXED (REJECTED)" | ✅ IMPLEMENTED — ScalarSubquery OpTree + operator | 🔴 Wrong |
| **Gap 4.2 (L419)** | "✅ FIXED (REJECTED)" | ✅ IMPLEMENTED — SemiJoin / AntiJoin OpTree + operators | 🔴 Wrong |
| **Gap 4.3 (L429)** | "✅ FIXED (REJECTED)" | ✅ IMPLEMENTED — SemiJoin / AntiJoin + equality condition | 🔴 Wrong |
| **Gap 4.4 (L439)** | "✅ FIXED (REJECTED)" | ANY_SUBLINK → IMPLEMENTED; ALL_SUBLINK → still rejected | 🟡 Partially wrong |
| **Phase B section** | "B1–B3 subqueries remain... 4-6 sessions estimated" | B1–B3 are complete | 🔴 Wrong |
| **Recommended Next Step** | "Continue with Phase B1 (EXISTS subqueries)" | B1-B3 done. Next should be Tier 3 | 🔴 Wrong |
| **"What Works Well" table** | No subquery WHERE rows | Missing SemiJoin/AntiJoin/ScalarSubquery entries | 🟡 Incomplete |

### ✅ Accurate Sections

| Section | Status |
|---------|--------|
| Gap 1 (Expressions 1.1–1.11) | ✅ All accurate |
| Gap 2 (Joins 2.1–2.5) | ✅ All accurate |
| Gap 3 (Aggregation 3.1–3.4) | ✅ Accurate (Phase 1+2+3 done, remaining listed correctly) |
| Gap 5 (LATERAL) | ✅ Accurate |
| Gap 6 (Clause features 6.1–6.8) | ✅ All accurate |
| Gap 7 (Window 7.1–7.4) | ✅ All accurate |
| Gap 8 (Data types 8.1–8.6) | ✅ All accurate |
| Gap 9 (Documentation 9.1–9.3) | ✅ Accurate |
| Priority 1–4 sections | ✅ Accurate |
| Phase A, Phase C | ✅ Accurate |

---

## Current Codebase State (Ground Truth)

### Supported Features (working in DIFFERENTIAL mode)

| Category | Feature | OpTree / Strategy |
|----------|---------|------------------|
| **Core** | Scan, Project, Filter | Scan, Project, Filter |
| **Joins** | INNER, LEFT, RIGHT, FULL, CROSS, nested 3+ | InnerJoin, LeftJoin, FullJoin |
| **Aggregation** | 22 AggFunc variants (COUNT/SUM/AVG/MIN/MAX + 17 group-rescan) | Aggregate |
| **Dedup** | DISTINCT | Distinct |
| **Set ops** | UNION ALL, UNION, INTERSECT [ALL], EXCEPT [ALL] | UnionAll, Intersect, Except |
| **Subqueries** | FROM subquery | Subquery |
| **Subqueries** | EXISTS / NOT EXISTS in WHERE | SemiJoin / AntiJoin |
| **Subqueries** | IN / NOT IN (subquery) in WHERE | SemiJoin / AntiJoin |
| **Subqueries** | Scalar subquery in SELECT list | ScalarSubquery |
| **CTEs** | Non-recursive WITH (single + multi-ref) | CteScan |
| **CTEs** | WITH RECURSIVE (FULL mode, recomputation in DIFF) | RecursiveCte |
| **Window** | Window functions (PARTITION BY, ORDER BY, frames, named) | Window |
| **LATERAL** | SRFs (unnest, jsonb_array_elements, etc.) | LateralFunction |
| **LATERAL** | Correlated subquery in FROM | LateralSubquery |
| **Expressions** | 30+ expression types (CASE, COALESCE, IN list, BETWEEN, etc.) | Expr variants |

### Explicitly Rejected Constructs (clear error messages)

| Construct | Error Behavior | Applies To |
|-----------|---------------|------------|
| LIMIT / OFFSET | Rejected — stream tables are full result sets | All modes |
| DISTINCT ON | Rejected → use DISTINCT or ROW_NUMBER() | All modes |
| NATURAL JOIN | Rejected → use explicit JOIN ... ON | All modes |
| GROUPING SETS / CUBE / ROLLUP | Rejected → use separate STs + UNION ALL, or FULL mode | All modes |
| FOR UPDATE / FOR SHARE | Rejected — no row-level locking | All modes |
| TABLESAMPLE | Rejected — use WHERE random() | All modes |
| ALL (subquery) | Rejected → use NOT EXISTS | All modes |
| SubLinks inside OR | Rejected → use UNION or separate STs | DIFF mode |
| Scalar subquery in WHERE | Rejected → use JOIN or CTE | DIFF mode |
| Mixed UNION / UNION ALL | Rejected → use all UNION or all UNION ALL | All modes |
| ROWS FROM() with multiple functions | Rejected → use single SRF | All modes |
| Different PARTITION BY across window funcs | Rejected → must share same PARTITION BY | DIFF mode |
| Window functions nested in expressions | Rejected → move to separate column | All modes |
| LATERAL with RIGHT/FULL JOIN | Rejected → only INNER/LEFT supported | All modes |
| Recursive CTE in DIFFERENTIAL | Rejected → use FULL mode | DIFF mode |
| 20 recognized-but-unsupported aggregates | Rejected → use FULL mode | DIFF mode |

### Aggregate Support Summary

| Implemented (25) | Recognized but Rejected (17) |
|------------------|------------------------------|
| COUNT, COUNT(*), SUM, AVG, MIN, MAX | XMLAGG |
| BOOL_AND/EVERY, BOOL_OR | CORR, COVAR_POP, COVAR_SAMP |
| STRING_AGG, ARRAY_AGG | REGR_AVGX/AVGY/COUNT/INTERCEPT/R2/SLOPE/SXX/SXY/SYY |
| JSON_AGG, JSONB_AGG | RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST (as aggregates) |
| BIT_AND, BIT_OR, BIT_XOR | |
| JSON_OBJECT_AGG, JSONB_OBJECT_AGG | |
| STDDEV_POP/STDDEV, STDDEV_SAMP | |
| VAR_POP, VAR_SAMP/VARIANCE | |
| MODE (ordered-set) | |
| PERCENTILE_CONT (ordered-set) | |
| PERCENTILE_DISC (ordered-set) | |

---

## Remaining Gaps — Prioritized

All remaining items are **P2 or lower** — nothing is silently broken. Every unsupported construct is either working correctly or rejected with a clear, actionable error message.

### Tier 1: Quick Wins (1–2 sessions, follow established patterns)

These use the proven group-rescan pattern — each is a copy-paste of an existing aggregate with minimal new logic.

| ID | Construct | Strategy | Effort | Impact |
|----|-----------|----------|--------|--------|
| **A1** | `MODE()` ordered-set aggregate | Group-rescan | ✅ **COMPLETE** | Medium — used in analytics |
| **A2** | `PERCENTILE_CONT()`, `PERCENTILE_DISC()` | Group-rescan | ✅ **COMPLETE** | Medium — used in reporting |
| **A3** | Regression aggregates: `CORR`, `COVAR_POP/SAMP`, `REGR_*` (11 functions) | Group-rescan | 4–6 hours | Low — niche statistical use |

A1 and A2 are complete: 3 new AggFunc variants (Mode, PercentileCont, PercentileDisc), WITHIN GROUP (ORDER BY) parsing, 17 new unit tests. 826 unit tests passing.

**Note:** 17 rejected aggregates remain (regression + hypothetical-set + XMLAGG). All can be implemented with group-rescan if demand arises.

### Tier 2: Structural Features (2–4 sessions, significant new logic)

These require new OpTree variants or major parser changes — not just following existing patterns.

| ID | Construct | Complexity | Effort | Impact |
|----|-----------|------------|--------|--------|
| **S1** | GROUPING SETS / CUBE / ROLLUP (full impl) | High — each grouping set = separate aggregation | 10–15 hours | Medium |
| **S2** | DISTINCT ON (full impl) | Medium — rewrite to ROW_NUMBER() OVER (...) = 1 | 6–8 hours | Medium |
| **S3** | Mixed UNION / UNION ALL | Medium — per-arm dedup flags | 4–6 hours | Low |
| **S4** | Multiple PARTITION BY in one query | High — multiple recomputation passes | 8–10 hours | Low |
| **S5** | ROWS FROM() with multiple functions | Low — zip SRF outputs | 3–4 hours | Very low |

### Tier 3: Edge Cases (optional, low priority)

| ID | Construct | Notes | Effort |
|----|-----------|-------|--------|
| **E1** | Scalar subquery in WHERE | Currently rejected; complex — would need value-change tracking per row | 6–8 hours |
| **E2** | SubLinks inside OR conditions | Requires OR-to-UNION rewrite; tricky for delta correctness | 8–10 hours |
| **E3** | ALL (subquery) | Dual of ANY — anti-join with universal quantification | 4–6 hours |
| **E4** | NATURAL JOIN (full impl) | Needs catalog access to resolve column lists at parse time | 6–8 hours |
| **E5** | Hypothetical-set aggregates (RANK, DENSE_RANK, PERCENT_RANK, CUME_DIST as aggregates) | Rare — almost always used as window functions, not aggregates | 4–6 hours |
| **E6** | XMLAGG | Very niche | 1–2 hours |

---

## GAP_SQL_OVERVIEW.md — Required Updates ✅ ALL COMPLETE

All corrections have been applied to GAP_SQL_OVERVIEW.md.

### Task R1: Fix Gap 4 (Subquery) status ✅ COMPLETE

**Applied:**
1. Gap 4 intro: Updated from "No subquery expression types produce valid SQL" to implementation status
2. Gap 4.1: Changed "✅ FIXED (REJECTED)" → "✅ IMPLEMENTED" with ScalarSubquery details
3. Gap 4.2: Changed "✅ FIXED (REJECTED)" → "✅ IMPLEMENTED" with SemiJoin/AntiJoin details
4. Gap 4.3: Changed "✅ FIXED (REJECTED)" → "✅ IMPLEMENTED" with rewrite details
5. Gap 4.4: Changed to "✅ PARTIALLY IMPLEMENTED" — ANY implemented, ALL still rejected
6. "Fix Options" section replaced with Implementation Summary table

### Task R2: Fix Executive Summary and metadata ✅ COMPLETE

**Applied:**
1. Last Updated: Now references PLAN_SQL_GAPS_3 Tasks 1-10 completion
2. Executive summary: Added subquery WHERE support to feature list
3. Removed subqueries from "Remaining gaps"
4. Test count updated to 809

### Task R3: Fix Phase B and Recommended Next Step ✅ COMPLETE

**Applied:**
1. Phase B: B1–B3 marked as ✅ IMPLEMENTED with details
2. "Estimated effort" and "sessions remaining" removed — Phase B fully complete
3. Recommended Next Step: Points to GAP_SQL_PHASE_4.md for remaining work

### Task R4: Update "What Works Well" table ✅ COMPLETE

**Added rows:**
| **Subqueries** | `WHERE EXISTS (SELECT ...)` / `NOT EXISTS` | ✅ | ✅ Semi-join / Anti-join |
| **Subqueries** | `WHERE col IN (SELECT ...)` / `NOT IN` | ✅ | ✅ Semi-join / Anti-join |
| **Subqueries** | Scalar subquery in SELECT `(SELECT max(x) FROM t)` | ✅ | ✅ Scalar subquery operator |

---

## Recommendations

### What to Do Next

**Option A: Fix GAP_SQL_OVERVIEW.md accuracy (Tasks R1-R4) — Recommended first**
- **Effort:** 1 hour
- **Rationale:** The report is the canonical reference. Having it say "REJECTED" for features that are fully implemented is misleading. This should be fixed before any new feature work.

**Option B: Ordered-set aggregates (A1-A2)**
- **Effort:** 5-7 hours (1 session)
- **Rationale:** MODE, PERCENTILE_CONT, PERCENTILE_DISC are the most commonly requested remaining aggregates. Group-rescan pattern is proven. Users requesting percentile reporting cannot currently use DIFFERENTIAL mode.

**Option C: DISTINCT ON full implementation (S2)**
- **Effort:** 6-8 hours (1 session)
- **Rationale:** The rewrite-to-window-function approach is clean and well-understood. DISTINCT ON is a common PostgreSQL idiom for "first row per group" queries. Currently rejected.

**Option D: Regression aggregates (A3)**
- **Effort:** 4-6 hours (1 session)
- **Rationale:** 11 functions (CORR, COVAR_*, REGR_*) follow the same group-rescan pattern. Low individual demand but covers a large number of gap items at once.

### Recommended Execution Order

```
Session 1: Tasks R1-R4 (REPORT fix) + A1-A2 (MODE + PERCENTILE)
Session 2: S2 (DISTINCT ON) or A3 (regression aggregates)
Session 3: S3 (Mixed UNION/UNION ALL)
Session 4+: S1 (GROUPING SETS) — only if specifically requested
```

### What NOT to Prioritize

These items have low user demand and/or high complexity relative to their value:

| Item | Why Defer |
|------|-----------|
| **GROUPING SETS full impl (S1)** | 10-15 hours, needs new OpTree variant, rarely requested. Rejection with UNION ALL alternative is adequate. |
| **Multiple PARTITION BY (S4)** | 8-10 hours, complex multi-pass recomputation. Edge case. |
| **ROWS FROM multi-function (S5)** | Very niche — almost never used. |
| **NATURAL JOIN full impl (E4)** | Needs catalog access at parse time. Rejection is appropriate — explicit JOINs are better practice. |
| **SubLinks inside OR (E2)** | OR-to-UNION rewrite is architecturally complex and error-prone. |
| **Hypothetical-set aggregates (E5)** | Almost always used as window functions, not aggregates. Rejection is fine. |

---

## Success Criteria

After this plan's recommended work (Sessions 1-2):

- [x] GAP_SQL_OVERVIEW.md is 100% accurate — no stale claims ✅ DONE (Option A, Tasks R1-R4)
- [x] 25 AggFunc variants (up from 22): added MODE, PERCENTILE_CONT, PERCENTILE_DISC ✅ DONE
- [ ] OR: DISTINCT ON works via window-function rewrite
- [x] 826 unit tests (target was 825+) ✅ DONE
- [ ] Documentation fully consistent across README, SQL_REFERENCE, DVM_OPERATORS, REPORT_SQL_GAPS

---

## Historical Progress Summary

| Plan | Sessions | Items Resolved | Test Growth |
|------|----------|---------------|-------------|
| PLAN_SQL_GAPS_1 | 1 | Window detection, CROSS JOIN tests, FOR UPDATE rejection, report cleanup | 745 → 757 |
| PLAN_SQL_GAPS_2 | 1 | GROUPING SETS P0 fix, GROUP BY hardening, TABLESAMPLE rejection | 745 → 750 |
| PLAN_SQL_GAPS_3 | ~5 | 5 new aggregates + 3 subquery operators (SemiJoin, AntiJoin, ScalarSubquery) | 750 → 809 |
| PLAN_SQL_GAPS_4 | 1 | Report accuracy fix (R1-R4) + 3 ordered-set aggregates (A1-A2) | 809 → 826 |
| **Total** | **~8** | **47+ of 52 original gaps resolved** | **~700 → 826** |

**Remaining:** 8 gap items remain as intentional rejections (GROUPING SETS, DISTINCT ON, Mixed UNION, Multiple PARTITION BY, ROWS FROM, NATURAL JOIN) + 20 aggregate functions recognized but rejected. All have clear error messages with actionable rewrite suggestions. Zero P0 or P1 issues remain.
