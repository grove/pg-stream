# PLAN_PERFORMANCE_PART_8.md — Residual Bottlenecks & Next-Wave Optimizations

## Current Benchmark Results (2026-02-22)

### Full Matrix — Summary (avg ms per cycle)

| Scenario   | Rows   | Chg % | FULL ms | INCR ms     | INCR c1 | INCR 2+ | INCR med | INCR P95 |
|------------|--------|-------|---------|-------------|---------|---------|----------|----------|
| scan       | 10K    | 1%    | 32.9    | **1.4** (22.8x) | 1.1  | 1.5     | 1.1      | 2.7      |
| scan       | 10K    | 10%   | 29.2    | **2.6** (11.2x) | 3.8  | 2.5     | 2.4      | 4.4      |
| scan       | 10K    | 50%   | 37.0    | 36.7 (1.0x)     | 28.5 | 37.6    | 27.6     | 74.0     |
| scan       | 100K   | 1%    | 300.1   | **4.6** (65.9x) | 1.8  | 4.9     | 1.7      | 9.2      |
| scan       | 100K   | 10%   | 265.2   | 111.5 (2.4x)    | 113.0| 111.3   | 116.3    | 144.6    |
| scan       | 100K   | 50%   | 548.8   | 821.7 (0.7x)    | 881.2| 815.1   | 841.4    | 956.0    |
| filter     | 10K    | 1%    | 21.7    | **4.8** (4.6x)  | 2.1  | 5.1     | 4.9      | 9.3      |
| filter     | 10K    | 10%   | 19.4    | **3.4** (5.7x)  | 1.6  | 3.6     | 1.8      | 7.6      |
| filter     | 10K    | 50%   | 18.5    | 21.5 (0.9x)     | 15.0 | 22.2    | 15.9     | 47.8     |
| filter     | 100K   | 1%    | 134.4   | **3.4** (39.5x) | 1.9  | 3.6     | 1.7      | 9.8      |
| filter     | 100K   | 10%   | 151.7   | 82.5 (1.8x)     | 84.4 | 82.3    | 83.0     | 92.0     |
| filter     | 100K   | 50%   | 212.1   | 175.4 (1.2x)    | 191.4| 173.7   | 180.6    | 206.8    |
| aggregate  | 10K    | 1%    | 4.4     | **2.3** (1.9x)  | 2.7  | 2.2     | 1.7      | 4.3      |
| aggregate  | 10K    | 10%   | 7.5     | **1.9** (4.0x)  | 1.8  | 1.9     | 1.7      | 2.8      |
| aggregate  | 10K    | 50%   | 7.6     | 8.5 (0.9x)      | 5.8  | 8.8     | 5.5      | 22.4     |
| aggregate  | 100K   | 1%    | 17.2    | **2.5** (6.8x)  | 2.4  | 2.5     | 1.7      | 4.6      |
| aggregate  | 100K   | 10%   | 18.3    | 10.7 (1.7x)     | 19.7 | 9.6     | 9.4      | 18.8     |
| aggregate  | 100K   | 50%   | 25.3    | 40.4 (0.6x)     | 30.6 | 41.4    | 31.4     | 70.9     |
| join       | 10K    | 1%    | 38.4    | **11.1** (3.5x) | 44.3 | 7.4     | 7.1      | 28.8     |
| join       | 10K    | 10%   | 29.6    | 33.2 (0.9x)     | 23.5 | 34.2    | 15.6     | 111.7    |
| join       | 10K    | 50%   | 32.9    | 28.8 (1.1x)     | 25.8 | 29.1    | 26.4     | 40.8     |
| join       | 100K   | 1%    | 294.0   | **18.0** (16.3x)| 20.1 | 17.8    | 17.5     | 21.0     |
| join       | 100K   | 10%   | 453.8   | 188.2 (2.4x)    | 151.5| 192.2   | 165.9    | 328.4    |
| join       | 100K   | 50%   | 348.1   | 320.2 (1.1x)    | 297.7| 322.6   | 308.8    | 362.4    |
| join_agg   | 10K    | 1%    | 6.1     | 6.0 (1.0x)      | 9.4  | 5.6     | 5.5      | 8.0      |
| join_agg   | 10K    | 10%   | 7.5     | 14.1 (0.5x)     | 13.9 | 14.1    | 12.6     | 18.7     |
| join_agg   | 10K    | 50%   | 8.1     | 10.2 (0.8x)     | 7.7  | 10.5    | 7.6      | 21.9     |
| join_agg   | 100K   | 1%    | 27.8    | **14.6** (1.9x) | 15.7 | 14.5    | 10.9     | 30.5     |
| join_agg   | 100K   | 10%   | 31.4    | 95.4 (0.3x)     | 98.4 | 95.0    | 92.3     | 111.1    |
| join_agg   | 100K   | 50%   | 33.1    | 35.1 (0.9x)     | 34.5 | 35.2    | 34.6     | 37.3     |

### Per-Phase Timing Breakdown (DIFFERENTIAL avg ms)

| Scenario   | Rows   | Chg %  | Decision | Gen+Build | Merge   | Cleanup | Path       |
|------------|--------|--------|----------|-----------|---------|---------|------------|
| scan       | 10K    | 1%     | 0.23     | 0.14      | 1.07    | 0.11    | cache_hit  |
| scan       | 10K    | 10%    | 0.28     | 0.09      | 2.24    | 0.22    | cache_hit  |
| scan       | 100K   | 1%     | 0.52     | 0.05      | 6.46    | 0.47    | cache_hit  |
| scan       | 100K   | 10%    | 1.53     | 0.06      | 100.30  | 7.74    | cache_hit  |
| filter     | 10K    | 1%     | 0.39     | 0.19      | 1.52    | 0.13    | cache_miss |
| filter     | 10K    | 10%    | 0.38     | 0.12      | 3.19    | 0.28    | cache_hit  |
| filter     | 100K   | 1%     | 0.49     | 0.12      | 6.24    | 0.48    | cache_hit  |
| filter     | 100K   | 10%    | 1.51     | 0.05      | 74.90   | 3.45    | cache_hit  |
| aggregate  | 10K    | 1%     | 0.30     | 0.15      | 0.82    | 0.12    | cache_hit  |
| aggregate  | 10K    | 10%    | 0.28     | 0.04      | 0.96    | 0.19    | cache_hit  |
| aggregate  | 100K   | 1%     | 0.46     | 0.08      | 1.89    | 0.39    | cache_hit  |
| aggregate  | 100K   | 10%    | 0.94     | 0.05      | 5.35    | 1.59    | cache_hit  |
| join       | 10K    | 1%     | 0.51     | 0.03      | 5.09    | 1.18    | cache_hit  |
| join       | 10K    | 10%    | 0.66     | 0.03      | 11.68   | 1.19    | cache_hit  |
| join       | 100K   | 1%     | 0.72     | 0.09      | 12.85   | 1.07    | cache_hit  |
| join       | 100K   | 10%    | 1.85     | 0.10      | 168.38  | 4.20    | cache_hit  |
| join_agg   | 10K    | 1%     | 0.48     | 0.09      | 2.64    | 0.65    | cache_hit  |
| join_agg   | 10K    | 10%    | 0.77     | 0.09      | 8.13    | 1.30    | cache_hit  |
| join_agg   | 100K   | 1%     | 0.71     | 0.09      | 6.67    | 0.88    | cache_hit  |
| join_agg   | 100K   | 10%    | 1.80     | 0.10      | 79.28   | 4.53    | cache_hit  |

### No-Data Refresh Latency

- **Avg: 9.73 ms** (target <10 ms: ✅ PASS, borderline)
- Max: 80.38 ms (cold start)

### Criterion Micro-Benchmarks (delta SQL generation, pure Rust)

| Benchmark                    | Time (µs) |
|------------------------------|-----------|
| diff_scan/3cols              | 9.9       |
| diff_scan/10cols             | 24.1      |
| diff_scan/20cols             | 47.7      |
| diff_filter                  | 11.3      |
| diff_project                 | 10.7      |
| diff_aggregate/count_star    | 9.3       |
| diff_aggregate/sum_count_avg | 21.7      |
| diff_inner_join              | 30.7      |
| diff_left_join               | 26.8      |
| diff_distinct                | 13.6      |
| diff_union_all/3_children    | 18.8      |
| diff_union_all/5_children    | 45.3      |
| diff_union_all/10_children   | 105.9     |
| diff_window_row_number       | 17.1      |
| diff_join_aggregate          | 54.2      |
| diff_cte_simple              | 16.1      |
| diff_lateral_srf             | 13.5      |

---

## Comparison with Previous Checkpoints

### Key Scenario: 100K rows, 1% changes

| Checkpoint       | scan INCR ms | filter INCR ms | join INCR ms | agg INCR ms | join_agg INCR ms |
|------------------|:------------:|:--------------:|:------------:|:-----------:|:----------------:|
| Baseline         | 572.4 (0.7x) | 126.0 (1.0x)  | 336.1 (1.0x) | 22.3 (1.3x) | 45.3 (1.2x)   |
| After P1+P2      | 140.3 (2.7x) | 68.8 (2.7x)   | 163.9 (2.7x) | 24.3 (1.4x) | 33.9 (1.5x)   |
| After P7         | 46.5 (7.0x)  | 34.0 (7.9x)   | 63.5 (6.0x)  | 10.9 (2.6x) | 21.6 (3.1x)   |
| After Part 6     | 8.3 (41.7x)  | 7.4 (26.3x)   | 12.3 (30.4x) | 5.7 (4.4x)  | 8.5 (4.9x)    |
| **Now (Part 8)** | **4.6 (65.9x)** | **3.4 (39.5x)** | **18.0 (16.3x)** | **2.5 (6.8x)** | **14.6 (1.9x)** |

### Observations vs Part 6

| Scenario      | Part 6 → Now | Notes |
|---------------|:------------:|-------|
| scan 100K/1%  | 8.3 → **4.6** (**1.8x faster**) | Continued improvement, now under 5ms |
| filter 100K/1%| 7.4 → **3.4** (**2.2x faster**) | Excellent; near-zero overhead |
| agg 100K/1%   | 5.7 → **2.5** (**2.3x faster**) | Big improvement |
| join 100K/1%  | 12.3 → **18.0** (**0.7x regression**) | Regression — investigate |
| join_agg 100K/1% | 8.5 → **14.6** (**0.6x regression**) | Regression — investigate |

### Pipeline Overhead (Decision + Gen+Build)

- **Consistently < 1ms** for cache hits across all scenarios at 1% change rate
- Gen+Build ≤ 0.19ms — delta SQL generation is negligible
- **MERGE dominates**: 70–97% of total time in every scenario

---

## Analysis & Bottleneck Identification

### B1: Join & Join_Agg Regressions (100K/1%)

**Symptom**: join 12.3→18.0ms (+46%), join_agg 8.5→14.6ms (+72%).

**Probable causes**:
- **Rescan CTE overhead**: The new rescan CTE (added for group-rescan aggregates) adds a LEFT JOIN to the merge CTE. Even for algebraic aggregates (SUM/COUNT), the CTE is structurally present — the planner must still evaluate `build_rescan_cte()` returning `None`, but the code should skip the JOIN entirely. Need to verify the generated SQL doesn't have any residual artifact.
- **Docker image changes**: The E2E image was rebuilt with all recent code changes. Compilation flags or shared library differences could cause planner behavior changes.
- **Benchmark variance**: Join P95 = 21.0ms with median 17.5ms. Previous Part 6 numbers had median = 13.2ms but that run used different Docker image builds on potentially different host load conditions. The join 10K/1% cycle-1 spike of 44.3ms (vs 7.4 on 2+) suggests cache warming variance.

**Action**: Profile the generated delta SQL for join 100K/1% to isolate whether the CTE structure changed, or if PostgreSQL planner behavior shifted.

### B2: join_agg Severe Regressions at 10%

**Symptom**: join_agg 100K/10% → 95.4ms (0.3x FULL). This is the worst scenario in the matrix.

**Root cause**: At 10%, ~10K change rows feed through the join+aggregate pipeline. The MERGE must:
1. Semi-join delta keys against the base table (10K keys)
2. Re-aggregate N groups
3. MERGE into the stream table

With only 5 groups, 10K changes touching all 5 groups means every group gets re-aggregated. The incremental path does strictly more work than FULL (which is a simple `TRUNCATE + INSERT INTO ... SELECT`).

**Potential fix**: **Adaptive fallback** should detect this and switch to FULL. Verify `auto_threshold` is triggering correctly for join_agg at 10%.

### B3: scan 100K/50% Severe Regression (0.7x)

**Symptom**: 821.7ms vs 548.8ms FULL. P95 = 956ms.

**Root cause**: At 50% change rate, 50K rows flow through the delta pipeline. The MERGE INTO with 50K delta rows competing against 100K existing rows is strictly more expensive than `TRUNCATE + INSERT INTO ... SELECT` with 100K rows (which is a simple sequential scan + load). The adaptive threshold should be catching this.

**Action**: Lower the adaptive threshold or ensure it is operational for scan queries.

### B4: No-Data Latency at Target Boundary (9.73ms)

**Symptom**: Avg 9.73ms, barely passing the <10ms target. Max = 80.38ms.

**Analysis**: Previous measurements were ~3ms. The increase likely comes from:
- Additional SPI calls in the rescan CTE infrastructure (checking for group-rescan aggregates)
- Changed Docker host conditions
- Benchmark run variance

**Action**: Re-run with more iterations. If consistently >5ms, profile the no-data path.

### B5: Aggregate P95 Instability (10K/50%)

**Symptom**: agg 10K/50% median = 5.5ms but P95 = 22.4ms (4x spike).

**Root cause**: PostgreSQL plan cache invalidation. Every Nth execution triggers re-planning, which costs ~10-15ms. With 50% changes affecting all 5 groups, the merge complexity varies across cycles.

### B6: MERGE Dominance — The Fundamental Limit

The per-phase breakdown shows MERGE execution accounts for:
- 1ms at 10K/1% (fast — minimal work)
- 5-13ms at 100K/1% (moderate)
- 74-168ms at 100K/10% (dominates total time)

The pipeline overhead (Decision + Gen+Build) is <1ms — essentially zero. **Any further speedup must come from the MERGE SQL itself or from skipping it entirely.**

---

## Proposed Optimizations

### Phase A: Regression Investigation & Fixes (Priority: HIGH)

#### A-1: Verify rescan CTE not leaking into algebraic aggregate SQL

**Goal**: Confirm that `diff_aggregate` for SUM/COUNT/AVG does NOT add a rescan CTE or LEFT JOIN.

**Steps**:
1. Add a unit test asserting no `agg_rescan` appears in `diff_aggregate` output for SUM/COUNT queries
2. If found, fix `build_rescan_cte` to return `None` more efficiently for algebraic-only aggregate lists

**Effort**: 30 min. **Impact**: May explain the join_agg 100K/1% regression.

#### A-2: Profile join 100K/1% delta SQL

**Goal**: Compare the generated MERGE SQL from Part 6 vs now.

**Steps**:
1. Add `[PGS_DEBUG]` SQL logging for join queries
2. Run benchmark, capture the full delta SQL
3. Compare CTE structure with `/target/criterion/` historical baselines
4. Check if any new CTEs were introduced

**Effort**: 1 hour. **Impact**: Diagnose +46% join regression.

#### A-3: Audit adaptive threshold for regression scenarios

**Goal**: Verify `auto_threshold` triggers FULL fallback for join_agg 10K/10% and scan 100K/50%.

**Steps**:
1. Add `[PGS_PROFILE]` logging showing threshold decisions
2. Verify that after 2-3 cycles of INCR being slower, the threshold auto-adjusts
3. If not triggering, lower the default threshold or fix the calculation

**Effort**: 1 hour. **Impact**: Should eliminate 0.3x-0.7x scenarios by falling back to FULL.

### Phase B: MERGE Optimization (Priority: HIGH)

#### B-1: Conditional MERGE bypass for no-change groups (aggregates)

**Problem**: For algebraic aggregates with 5 groups (SUM/COUNT), every delta cycle re-checks all 5 groups even if some have zero changes. The MERGE must read from `delta`, LEFT JOIN the stream table, compute new values, and write for each group — even when `new_count = old_count` AND `new_sum = old_sum`.

**Fix**: Add a pre-MERGE filter CTE that eliminates unchanged groups before the MERGE:

```sql
-- Before MERGE, filter the final CTE to only groups with actual changes
WITH __pgs_changed_groups AS (
    SELECT * FROM __pgs_cte_agg_final_N
    WHERE __pgs_action IN ('I', 'D')  -- already done
)
MERGE INTO st USING __pgs_changed_groups ...
```

This is already present (the `WHERE ... IS DISTINCT FROM` guard). The issue may be that PostgreSQL MERGE evaluates all matched rows even when the WHEN clause filters them. Consider splitting to `DELETE + INSERT` with pre-filtered CTEs instead of MERGE.

**Effort**: 2 hours. **Impact**: 10-30% at low change rates where many groups are unchanged.

#### B-2: Partitioned MERGE for large deltas

**Problem**: At 100K/10%, the MERGE processes 10K delta rows in a single SQL statement. PostgreSQL's planner may choose a nested-loop join strategy that scales poorly.

**Fix**: For delta sizes > 1000 rows, split into batched MERGEs by group key hash or row_id range:

```sql
-- Instead of one MERGE with 10K rows:
MERGE INTO st USING (SELECT * FROM delta WHERE __pgs_row_id % 4 = 0) ...
MERGE INTO st USING (SELECT * FROM delta WHERE __pgs_row_id % 4 = 1) ...
-- etc.
```

**Effort**: 3 hours. **Impact**: Speculative — depends on planner behavior. Risk of increased overhead from multiple statements.

**Decision**: Defer until B-1 and A-3 are evaluated.

#### B-3: Replace MERGE with DELETE + INSERT for large deltas

**Problem**: PostgreSQL MERGE has overhead from the WHEN MATCHED/NOT MATCHED branching and tuple visibility checks. For scenarios where most rows change, `DELETE changed_rows; INSERT new_rows` may be cheaper.

**Fix**: When delta covers >25% of stream table rows, use:
```sql
DELETE FROM st WHERE (key_cols) IN (SELECT key_cols FROM delta);
INSERT INTO st SELECT ... FROM delta WHERE __pgs_action = 'I';
```

**Effort**: 2 hours. **Impact**: Potentially significant at 50% — eliminates MERGE planning overhead. Risk: two statements vs one.

**Decision**: Implement behind a GUC flag (`pg_stream.merge_strategy = 'auto'|'merge'|'delete_insert'`).

### Phase C: Cleanup & Buffer Optimization (Priority: MEDIUM)

#### C-1: Async change buffer cleanup

**Problem**: Cleanup takes 0.5-7.7ms at 100K. Currently synchronous via `TRUNCATE`.

**Fix**: Move cleanup to a deferred callback or background worker tick. The change buffer is bounded by LSN range, so stale rows are harmless as long as the next refresh uses the correct frontier.

**Effort**: 2 hours. **Impact**: Saves 0.5-7ms per refresh, critical path reduction.

#### C-2: Trigger write amplification reduction

**Problem**: At high write rates, every source DML fires a trigger that INSERTs into the change buffer. This means every source INSERT/UPDATE/DELETE generates:
- 1 WAL record for the source table
- 1 trigger execution
- 1 INSERT into the change buffer (+ WAL record + index maintenance)

**Fix**: **Statement-level triggers with transition tables** (PostgreSQL AFTER STATEMENT with referencing OLD/NEW TABLE). This batches all changes from a single statement into one buffer INSERT:

```sql
CREATE TRIGGER pg_stream_cdc_tr AFTER INSERT OR UPDATE OR DELETE
ON source_table REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows
FOR EACH STATEMENT EXECUTE FUNCTION pg_stream_cdc_stmt_fn();
```

**Effort**: 8 hours (requires CDC rewrite). **Impact**: 50-80% reduction in trigger overhead at high write volumes. No change for single-row DML.

**Decision**: Defer to Part 9 — this is a major CDC architecture change.

### Phase D: Planner Hints & Stability (Priority: MEDIUM)

#### D-1: Explicit join strategy hints for MERGE

**Problem**: P95 spikes in join scenarios (join 10K/10% P95 = 111.7ms vs median = 15.6ms) suggest planner instability.

**Fix**: Add `SET LOCAL` hints before MERGE execution:
```sql
SET LOCAL enable_nestloop = off;  -- for large deltas
SET LOCAL work_mem = '64MB';      -- for hash joins
```

Apply conditionally based on estimated delta size:
- delta < 100 rows: no hints (let planner optimize for small data)
- delta 100-10K rows: `enable_nestloop = off`
- delta > 10K rows: `enable_nestloop = off` + `work_mem = 64MB`

**Effort**: 2 hours. **Impact**: Should reduce P95 variability by 50%+.

#### D-2: SPI prepared statements

**Problem**: Every MERGE execution parses the SQL string. With template caching, the SQL is identical across cycles (only LSN placeholders differ), but PostgreSQL still parses it fresh each time.

**Fix**: Use `SPI_prepare()` + `SPI_execute_plan()` for the MERGE statement. Cache the plan handle alongside the SQL template.

**Caveat**: Previous attempt (H-W2 in Part 4) showed net-negative results because PostgreSQL's custom plan mode re-plans every EXECUTE for the first 5 executions. Need ≥6 cycles per stream table before the generic plan locks in. With CYCLES=10 this should work.

**Effort**: 4 hours. **Impact**: Saves ~1-2ms parse time per refresh. Higher impact at low change rates where parse is a larger fraction of total time.

**Decision**: Implement and benchmark with CYCLES=20 to amortize plan cache warmup.

### Phase E: No-Data Path Optimization (Priority: LOW)

#### E-1: Ultra-fast empty-buffer check

**Problem**: No-data latency = 9.73ms. Target is <10ms (barely passing).

**Fix**: Replace the current `count(*)` decision query with an `EXISTS` check that short-circuits on the first row:
```sql
SELECT EXISTS(
    SELECT 1 FROM pgstream_changes.changes_OID
    WHERE lsn > $prev_lsn AND lsn <= $new_lsn
    LIMIT 1
)
```

If this returns FALSE, skip all subsequent processing (no Gen, no Build, no MERGE, no Cleanup).

**Effort**: 1 hour. **Impact**: Should bring no-data from ~10ms to <3ms.

#### E-2: Shared memory fast-path for zero-change detection

**Problem**: Even the EXISTS check requires an SPI call into PostgreSQL.

**Fix**: Maintain a per-source-table atomic counter in shared memory (via `PgAtomic`). The CDC trigger increments it on every write. The refresh function reads it — if zero since last refresh, skip SPI entirely.

**Effort**: 3 hours. **Impact**: No-data latency → <1ms. Requires shared memory initialization.

**Decision**: Implement E-1 first; E-2 only if E-1 is insufficient.

---

## Implementation Priority & Schedule

### Session 1: Regression Triage (A-1, A-2, A-3) — ✅ COMPLETED

**A-1: Rescan CTE not leaking** — ✅ CONFIRMED. Added 6 unit tests verifying
no `agg_rescan` CTE is generated for SUM, COUNT(*), AVG, MIN, MAX, and
SUM+COUNT+AVG combined queries. The `build_rescan_cte` function correctly
returns `None` for algebraic-only aggregate lists. The rescan CTE infrastructure
does NOT contribute to the join/join_agg regressions.

**A-2: Join regression root cause** — ✅ IDENTIFIED. The join 100K/1% regression
(12.3ms → 18.0ms) is a pre-existing issue from the PREPARE/EXECUTE revert
(P1+P2 commits). The 11-CTE join delta query incurs significant planning
overhead when planned fresh every cycle. This is NOT caused by recent changes
(rescan CTE, TABLESAMPLE rejection, test fixes). Recovery requires restoring
SPI prepared statements (Session 5, D-2).

**A-3: Adaptive threshold bug** — ✅ FIXED. `last_full_ms` was never set during
initial materialization, keeping it NULL forever for STs whose change rate
never exceeded the 15% fallback threshold. The auto-tuner code (`if let
Some(last_full) = st.last_full_ms`) was dead code for these STs. Fixed by
recording the initial materialization time as `last_full_ms` during
`create_stream_table`. This enables the auto-tuner from the first differential
refresh onward, which will correctly lower the threshold for scenarios like
join_agg at 10% where INCR is slower than FULL.
Also added debug logging when the auto-tuner adjusts the threshold.

### Session 2: No-Data & Cleanup Fast Path (E-1, C-1) — ✅ COMPLETED

**E-1: Ultra-fast EXISTS no-data short-circuit**
- Replaced heavy LATERAL+capped-count decision query with two-phase approach
- Phase 1: Fast `SELECT EXISTS(...)` check — single SPI call for single-source;
  `UNION ALL` wrapped in `EXISTS()` for multi-source (short-circuits on first row)
- Phase 2: Capped-count threshold check only runs when changes actually exist,
  with early `break` on first source exceeding the FULL fallback threshold
- **Expected outcome**: No-data path avoids pg_class lookup and CASE expression;
  should bring no-data latency well under 5ms

**C-1: Deferred change buffer cleanup**
- Added `PendingCleanup` struct + `PENDING_CLEANUP` thread-local queue
- Cleanup is now enqueued (near-zero cost) instead of executing inline
- `drain_pending_cleanups()` runs at the start of the NEXT refresh cycle
- Safety: LSN-range predicates in delta queries ensure stale rows are never
  re-consumed, so deferred cleanup is fully safe
- Profiling label updated from `cleanup` to `cleanup_enqueue` to reflect deferral
- **Expected outcome**: 0.5–7ms savings on every differential refresh

### Session 3: Planner Stability (D-1) — ✅ COMPLETED

**D-1: Conditional SET LOCAL planner hints based on delta size**
- Added `apply_planner_hints()` function with three tiers:
  - delta < 100 rows: no hints (let planner optimise for small data)
  - delta 100–9 999: `SET LOCAL enable_nestloop = off` (favour hash joins)
  - delta >= 10 000: also `SET LOCAL work_mem = '<N>MB'` (avoid disk-spill)
- `SET LOCAL` is automatically reset at transaction end — no cleanup needed
- Accumulated `total_change_count` from the capped-count threshold loop to
  feed the hint tier decision
- New GUCs:
  - `pg_stream.merge_planner_hints` (bool, default true) — master switch
  - `pg_stream.merge_work_mem_mb` (int, default 64) — work_mem for large deltas
- Profiling line now includes `delta_est=<N>` and `hints=<tier>` fields
- **Expected outcome**: P95 reduction for join/join_agg scenarios where
  nested-loop plans cause latency spikes

### Session 4: MERGE Strategy (B-1, B-3) — ✅ COMPLETED

**B-1: IS DISTINCT FROM guard to skip no-op UPDATEs**
- Added `IS DISTINCT FROM` check on the MERGE `WHEN MATCHED ... THEN UPDATE`
  clause so unchanged rows are skipped entirely (no heap write)
- The guard is: `AND (st.col1 IS DISTINCT FROM d.col1 OR st.col2 IS DISTINCT FROM d.col2 OR ...)`
- Applied in both the `prewarm_merge_cache` path and the cache-miss build path
- **Expected outcome**: Eliminates unnecessary I/O for aggregate groups whose
  recomputed values are identical to the current values

**B-3: DELETE + INSERT alternative for large deltas (behind GUC)**
- New GUC: `pg_stream.merge_strategy` (string: `auto`/`merge`/`delete_insert`)
- In `auto` mode (default), switches to DELETE+INSERT when the estimated delta
  exceeds 25% of the source table row count (`MERGE_STRATEGY_AUTO_THRESHOLD`)
- DELETE+INSERT is two statements:
  1. `DELETE FROM st WHERE __pgs_row_id IN (SELECT __pgs_row_id FROM delta)`
  2. `INSERT INTO st SELECT ... FROM delta WHERE __pgs_action = 'I'`
- Both MERGE and DELETE+INSERT templates are cached alongside each other in
  `CachedMergeTemplate`, with strategy selection at execution time
- Profiling line now includes `strategy=merge|delete_insert` field
- Also accumulated `total_table_size` from the capped-count threshold loop
  to feed the auto-strategy decision
- **Expected outcome**: 10–30% improvement at 50% change rates where MERGE's
  tuple visibility overhead dominates

### Session 5: Prepared Statements (D-2) ✅ COMPLETED
- SQL PREPARE / EXECUTE for MERGE (not C-level SPI_prepare)
- New GUC: `pg_stream.use_prepared_statements` (default true)
- Parameterized MERGE template with `$N` positional params for LSN values
- PREPARE issued on first cache-hit, EXECUTE on subsequent cycles
- PostgreSQL switches from custom → generic plan after ~5 executions
- DEALLOCATE on cache invalidation; session-level lifetime
- 9 new unit tests (parameterize_lsn_template, build_prepare_type_list, build_execute_params)
- 841 unit tests passing, fmt+lint clean
- **Expected outcome**: 1-2ms savings per refresh on cache hits

---

## Targets for Part 8

| Metric                    | Current        | Target          |
|---------------------------|----------------|-----------------|
| scan 100K/1%              | 4.6ms (65.9x)  | < 5ms (maintain)|
| filter 100K/1%            | 3.4ms (39.5x)  | < 4ms (maintain)|
| aggregate 100K/1%         | 2.5ms (6.8x)   | < 3ms (maintain)|
| join 100K/1%              | 18.0ms (16.3x) | < 12ms (recover)|
| join_agg 100K/1%          | 14.6ms (1.9x)  | < 10ms (recover)|
| join_agg 100K/10%         | 95.4ms (0.3x)  | > 1.0x (fix)    |
| scan 100K/50%             | 821.7ms (0.7x) | > 1.0x (fix)    |
| No-data latency           | 9.73ms          | < 5ms           |
| P95 / median ratio        | up to 7.6x      | < 3x            |

---

## Summary of Changes Since Part 7

Since the Part 6/7 benchmarks, the following code changes were made that may affect performance:

1. **Rescan CTE** (`src/dvm/operators/aggregate.rs`): Added `build_rescan_cte()`, `child_to_from_sql()`, `agg_to_rescan_sql()` for group-rescan aggregates. This adds a LEFT JOIN to the merge CTE for group-rescan aggregates (BIT_AND, STDDEV, etc). For algebraic aggregates (SUM/COUNT/AVG), `build_rescan_cte` returns `None` and no JOIN is added. The standard benchmark queries (SUM+COUNT) should not be affected, but needs verification (A-1).

2. **TABLESAMPLE rejection** (`src/dvm/parser.rs`): Added `T_RangeTableSample` check. No performance impact.

3. **Test changes**: Updated E2E tests for PERCENTILE_CONT, STDDEV, EXISTS, window functions, JSON_OBJECT_AGG→JSONB_OBJECT_AGG. No runtime performance impact.

4. **Docker image rebuild**: The E2E image was rebuilt with all changes. Different `cargo build` compilation could produce different binary characteristics.
