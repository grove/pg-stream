# PLAN: Multi-Table Delta Batching (B-3)

## Status: 🟢 DONE

**Goal:** `v0.9.0` (Milestone 3)
**Remaining to do:** 
- [x] **B3-3:** Write property-based correctness proofs for simultaneous multi-source changes (diamond-flow scenarios) using Z-set weight aggregation (`SUM(weight)`) instead of `DISTINCT ON` deduplication.
- [x] **B3-2:** Implement merged-delta generation logic using Z-set block aggregation (`SUM(CASE WHEN action THEN 1...`) replacing DISTINCT ON.
- [x] **B3-1:** Implement intra-query delta branch pruning (skip `UNION ALL` branches entirely when a source has zero changes).

---

## Overview

When a stream table joins multiple tables (A, B, C) and multiple tables change in the same scheduler cycle, `pg_trickle` currently evaluates the DVM delta query using potentially separate passes or constructs unions with empty branches.

This plan outlines the steps to merge these passes into one optimized query plan, while mathematically preserving correctnesThis plan outlines the steps to merge these passes into one optimiz`DISTINCT ON`

If an update hits both Table If an update hits both Tab saIf an update hits both Table Ifble joins them, a single canonical row might be corrected twice (once from the ΔA path, once from the ΔB path). 

Previously, it was proposed to use `DISTINCT ON` for cross-delta deduplication. **This causes silent data corruptiPreviously, it was proposed to use `DISTINCT ON` for cross-delta deduplication. **This causes silent data corrupstPreviously, it was proposed to use `DISTINCT ON` for cross-delta deduplication. **This causes silent data corruptiPreviously, a_APr UNPreviouslySELECT 'B' AS src, * FROM delta_B
),
final_delta AS (
  SELECT __pgt_row_id, SUM(weight) as weight, <other_cols>
  FROM merged_delta
  GROUP BY __pgt_row_id
  HAVING SUM(weight) != 0
)
```

## Implementation Sequence

### 1. Property-based Correctness Proofs (B3-3) `✅ Done`
*What:* Write extensive property tests simulating diamond flows (where changes hit multiple branches of a join simultaneously).
*Why:* To formally prove that `SUM(weight)*Why:* To formally prove that `SUM(weight)*Why:* To formally prove that `SUM(weihat `DISTINCT ON` introduced.
*Where:* `tests/e2e_diamond_tests.rs` (Added `test_diamond_flow_simultaneous_multi_source_update`).

### 2. Merged-Delta Generation (B3-2) `✅ Done`
*What:* Implement the true multi-source delta engine in `src/dvm/diff.rs`.
*Why:* Generates the `UNION ALL` + `GROUP BY` logic dynamically.
*Where:* `src/refresh.rs` — `build_weight_agg_using()` wraps the multi-source delta in `GROUP BY __pgt_row_id, SUM(weight) + HAVING <> 0`; `build_keyless_weight_agg()` handles keyless tables with `generate_series` expansion.

### 3. Intra-query Delta-Branch Pruning (B3-1) `🟢 Done`
*What:* Optimize the generated `UNION ALL` statements. If 3 tables are in a join but only 1 changed, completely skip gen*What:* Optimize the generated `UNION ALL` statements. Ifar*What:* Optimize tPo*What:* Optimize the gand execution latency.
*Where:*W`src/refresh.rs` checking `any_changes` per buffer.
