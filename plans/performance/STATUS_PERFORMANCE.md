# pg_trickle — Performance History

How has incremental (DIFFERENTIAL) refresh performance improved over the optimization sessions, and what is the current state?

---

## Benchmark Setup

All benchmarks use the same full matrix: **5 scenarios × 2 table sizes × 3 change rates = 30 combinations**, run inside isolated PostgreSQL 18.x Docker containers via Testcontainers. Each benchmark applies a realistic DML mix (INSERT/UPDATE/DELETE) per cycle and measures wall-clock refresh time.

| Scenario | Defining Query | Operators Tested |
|----------|---------------|------------------|
| **scan** | `SELECT id, region, category, amount, score FROM src` | Table scan only |
| **filter** | `SELECT id, region, amount FROM src WHERE amount > 5000` | Scan + filter |
| **aggregate** | `SELECT region, SUM(amount), COUNT(*) FROM src GROUP BY region` | Scan + group-by aggregate |
| **join** | `SELECT s.id, s.region, s.amount, d.region_name FROM src s JOIN dim d ON ...` | Scan + inner join |
| **join_agg** | `SELECT d.region_name, SUM(s.amount), COUNT(*) FROM src s JOIN dim d ON ... GROUP BY ...` | Scan + join + aggregate |

Table sizes: **10,000** and **100,000** rows. Change rates: **1%**, **10%**, **50%**.

---

## Snapshot Timeline

Four benchmark snapshots were captured across the optimization sessions:

| Snapshot | What Changed |
|----------|-------------|
| **Baseline** | No optimizations |
| **After P1+P2** | Phase A: `to_jsonb()` trigger serialization, drop unused columns, LSN bounds. Phase B: cached MERGE SQL + cached OpTree/delta template |
| **After P6** | + `LATERAL VALUES` aggregate rewrite, PK resolution, no-change short-circuit, EXISTS early-exit, prepared statements |
| **After P7** | + deferred cleanup, warm-cycle tuning |

---

## Optimization Details

### Phase A — Quick Wins (P1)

- **Fix 3:** Replaced `row_to_json()::text` with `to_jsonb()` in CDC trigger functions for faster serialization.
- **Fix 4:** Dropped unused `old_row_data` column from change buffer tables, reducing I/O.
- **Fix 11:** Added upper LSN bound to change buffer queries and cleanup, preventing unbounded scans.

### Phase B — Caching (P2)

- **Fix 9:** Cached compiled MERGE SQL across refresh cycles, eliminating repeated query planning (~30ms saved per refresh).
- **Fix 10:** Cached parsed OpTree and delta SQL template, avoiding redundant parsing and differentiation (~15ms saved per refresh).

### Phase C — Operator Improvements (P3–P6)

- **Fix 5:** Replaced 4-branch UNION ALL in aggregate final delta CTE with single-pass `LATERAL VALUES` expansion — eliminates 3 redundant scans of the merge CTE.
- **Fix 6:** Primary key columns resolved from `pg_constraint` at parse time and stored in `OpTree::Scan`, preferring real PKs over the non-nullable column heuristic.
- **No-change short-circuit:** When the change buffer is empty, skip MERGE entirely and return in <2ms.
- **EXISTS early-exit:** Before building the delta query, run a cheap `SELECT EXISTS(...)` on the change buffer to detect no-ops.
- **Prepared statements:** Reuse prepared MERGE statements across cycles to avoid repeated planning.

### Phase D — Cleanup & Tuning (P7)

- **Deferred cleanup:** Change buffer cleanup moved to after the MERGE completes, reducing lock contention during the critical path.
- **Warm-cycle tuning:** Improved steady-state performance for cycles 2+ after initial cold start.

---

## Historical Speedup: INCREMENTAL vs FULL

### At 1% Change Rate (Typical Production Workload)

| Scenario | Rows | Baseline | After P1+P2 | After P6 | After P7 (Current) |
|----------|------|----------|-------------|----------|---------------------|
| **scan** | 10K | 2.6× | 2.6× | 10.1× | 4.4× |
| **scan** | 100K | **0.7×** (slower!) | 2.7× | **41.7×** | 7.0× |
| **filter** | 10K | 1.9× | 1.7× | 5.4× | 3.3× |
| **filter** | 100K | 2.5× | 2.7× | **26.3×** | 7.9× |
| **aggregate** | 10K | 1.4× | 1.2× | 1.7× | 1.5× |
| **aggregate** | 100K | 1.3× | 1.4× | **4.4×** | 2.6× |
| **join** | 10K | 2.3× | 1.6× | 6.7× | 2.9× |
| **join** | 100K | 2.6× | 2.7× | **30.4×** | 6.0× |
| **join_agg** | 10K | 1.0× | 1.0× | 0.9× | 0.7× |
| **join_agg** | 100K | 1.3× | 1.5× | 4.9× | 3.1× |

### At 10% Change Rate

| Scenario | Rows | Baseline | After P1+P2 | After P6 | After P7 (Current) |
|----------|------|----------|-------------|----------|---------------------|
| **scan** | 10K | 2.1× | 2.2× | 6.4× | 5.0× |
| **scan** | 100K | 0.8× | 1.8× | 4.5× | 3.3× |
| **filter** | 10K | 1.4× | 2.2× | 3.3× | 1.9× |
| **filter** | 100K | 1.6× | 1.6× | 3.5× | 3.1× |
| **aggregate** | 10K | 0.9× | 0.8× | 1.2× | 1.1× |
| **aggregate** | 100K | 0.6× | 0.6× | 1.1× | 0.8× |
| **join** | 10K | 1.4× | 1.4× | 4.1× | 1.8× |
| **join** | 100K | 1.5× | 1.9× | 3.1× | 3.3× |
| **join_agg** | 10K | 0.8× | 0.7× | 1.8× | 0.9× |
| **join_agg** | 100K | 0.7× | 0.7× | 0.9× | 0.8× |

### At 50% Change Rate

| Scenario | Rows | Baseline | After P1+P2 | After P6 | After P7 (Current) |
|----------|------|----------|-------------|----------|---------------------|
| **scan** | 10K | 1.1× | 1.1× | 3.1× | 2.5× |
| **scan** | 100K | 0.9× | 0.9× | 0.9× | 0.8× |
| **filter** | 10K | 1.2× | 1.2× | 0.7× | 0.9× |
| **filter** | 100K | 1.1× | 1.8× | 1.0× | 1.1× |
| **aggregate** | 10K | 1.2× | 1.7× | 1.6× | 1.0× |
| **aggregate** | 100K | 0.9× | 0.9× | 0.8× | 0.6× |
| **join** | 10K | 1.1× | 1.1× | 1.1× | 1.0× |
| **join** | 100K | 1.1× | 1.1× | 0.9× | 0.9× |
| **join_agg** | 10K | 1.3× | 1.1× | 0.6× | 1.4× |
| **join_agg** | 100K | 0.7× | 0.9× | 0.8× | 0.7× |

---

## Current State Summary (After P7)

### At 1% Change Rate (Typical Production Workload)

| Scenario | 10K rows | 100K rows |
|----------|----------|-----------|
| scan | **4.4×** faster | **7.0×** faster |
| filter | **3.3×** faster | **7.9×** faster |
| aggregate | **1.5×** faster | **2.6×** faster |
| join | **2.9×** faster | **6.0×** faster |
| join+agg | 0.7× (overhead) | **3.1×** faster |

### At 10% Change Rate

| Scenario | 10K rows | 100K rows |
|----------|----------|-----------|
| scan | **5.0×** faster | **3.3×** faster |
| filter | **1.9×** faster | **3.1×** faster |
| aggregate | 1.1× | 0.8× |
| join | **1.8×** faster | **3.3×** faster |
| join+agg | 0.9× | 0.8× |

### At 50% Change Rate

At 50% churn, INCREMENTAL approaches or slightly exceeds FULL cost across most scenarios (0.6–2.5×). This is expected — when half the table changes, the delta is nearly as expensive as a full recompute.

---

## Absolute Timings (Current, 100K Rows, 1% Changes)

| Scenario | FULL avg ms | INCREMENTAL avg ms | Speedup |
|----------|-------------|---------------------|---------|
| scan | 326 | 47 | 7.0× |
| filter | 267 | 34 | 7.9× |
| aggregate | 28 | 11 | 2.6× |
| join | 384 | 64 | 6.0× |
| join_agg | 67 | 22 | 3.1× |

---

## Interpretation

### What the Numbers Show

1. **Baseline was rough** — INCREMENTAL was actually *slower* than FULL for scan/100K (0.7×) and several other combos were near 1.0×. The overhead of delta computation, MERGE, and change buffering exceeded the cost of just re-running the query.

2. **P1+P2 (caching)** fixed the worst regressions. Caching the compiled MERGE SQL and parsed OpTree eliminated ~45ms of repeated planning per cycle. Large-table scan went from 0.7× to 2.7×.

3. **P6 was the breakthrough** — no-change short-circuit, EXISTS early-exit, and prepared statements produced the most dramatic gains. At 1% changes on 100K rows:
   - scan: **41.7×** (346ms → 8.3ms)
   - filter: **26.3×** (196ms → 7.4ms)
   - join: **30.4×** (374ms → 12.3ms)
   
   These extreme multipliers reflect that in a 10-cycle run, most cycles have **no changes** (the DML happens once, then subsequent cycles find empty change buffers and short-circuit in <2ms).

4. **P7 (current)** shows somewhat lower peak numbers than P6 due to benchmark variability (different run, cold-start cycle 1 weighing on averages) and the addition of deferred cleanup overhead. The steady-state "cycle 2+" numbers in the P6 detailed output are the most representative of real-world performance.

### Theoretical Basis

The larger the table and the smaller the change rate, the bigger the INCREMENTAL win. Differential maintenance is O(Δ) while full refresh is O(N), so the ratio improves as N/Δ grows. At 100K rows / 1% changes the system processes ~1,000 changed rows instead of re-scanning 100,000, yielding **3–8× speedups** in practice (and up to 30–40× when most cycles are no-ops).

### When INCREMENTAL Doesn't Help

- **50% change rate on large tables:** The delta is nearly as big as the table itself.
- **Small aggregates (join_agg at 10K):** The FULL refresh is already ~12ms; the overhead of delta computation, MERGE template, and cleanup exceeds the savings.
- **Aggregate 100K at 10–50%:** Group-rescan aggregates require reading the stream table to reconstruct counts, which at high change rates approaches full-table cost.

---

## Running Benchmarks

```bash
# Build the E2E test image first
./tests/build_e2e_image.sh

# Quick spot check
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_scan_10k_1pct

# Full 30-combination matrix (~15–30 minutes)
cargo test --test e2e_bench_tests --features pg18 -- --ignored --nocapture bench_full_matrix
```

See [BENCHMARK.md](../../docs/BENCHMARK.md) for full details on the benchmark harness.

---

## Design Decisions & Root Causes

This section preserves the key technical decisions and root-cause findings from the performance analysis phases (PLAN_PERFORMANCE through PLAN_PERFORMANCE_PART_7). These files were deleted after extracting the information below.

### Original Root Cause (Phase 0)

The initial benchmark showed INCREMENTAL was *slower* than FULL for scan/100K (0.7×). Root cause: `execute_manual_refresh()` ignored the `refresh_mode` parameter and always performed TRUNCATE + INSERT — effectively comparing FULL vs FULL with extra overhead. This was the single most impactful bug discovered.

### MERGE vs Temp Table (Phase A)

The original incremental apply used a 3-step SPI sequence: create temp table, DELETE matching rows, INSERT new rows. This was replaced with a single PostgreSQL `MERGE` statement that handles INSERT/UPDATE/DELETE in one operation. Result: 3 SPI calls → 1, ~30% less I/O, atomic application.

### JSONB Serialization Round-Trip (Phases A–B)

JSONB serialization was identified as the **dominant bottleneck** — not query planning as initially suspected. The full path: `row → JSON text → binary JSONB → store → read → typed row`. Two key fixes:

1. **Trigger serialization**: Replaced `row_to_json()::jsonb` with `to_jsonb(NEW)` — eliminates the text→binary conversion step.
2. **Row reconstruction**: Replaced per-column `->>'col'` extraction with `jsonb_populate_record()` — a single function call reconstructs the entire row instead of N separate field extractions.

### SPI Call Reduction (Phases B–C)

The no-op refresh path originally executed **7+ SPI round-trips** (13+ through the scheduler) even with zero pending changes. Key eliminations:

- **Redundant `SELECT count(*)`**: A full sequential scan of every change buffer was purely informational and served no purpose in the trigger-based CDC model. Replaced with `SELECT EXISTS(... LIMIT 1)`.
- **Combined SPI queries**: `reltuples` + capped-count queries merged via `LATERAL`. `store_frontier` + `complete_refresh` merged into single UPDATE.
- **No-op latency**: 86ms → 14.8ms → sub-2ms.

### Why PREPARE/EXECUTE Was Rejected (Phase D)

Prepared statements (`PREPARE` + `EXECUTE`) were implemented and then **reverted**. Root cause analysis:

1. **Custom plans (first ≤5 executions)** have the **same planning cost** as direct `SPI_execute()`, plus the overhead of looking up the prepared statement and binding arguments. No benefit for short-cycle benchmarks (CYCLES ≤ 5).
2. **Generic plans (≥6 executions)** skip re-planning but use **wrong selectivity** for LSN range predicates — without concrete LSN values, the planner uses average-case cardinality estimates, leading to wrong sort strategies for GROUP BY and join methods.
3. **String substitution** with concrete parameter values provides correct plans cheaply on every call.

### Scan Window Function Fast Path (Phase E)

At 1% changes, ~95%+ of PKs have exactly one change per refresh cycle. The expensive `FIRST_VALUE`/`LAST_VALUE` window functions (requiring full partition sort) are unnecessary for these single-change PKs. A 4-CTE split pipeline (`pk_stats → single → multi_raw → scan_raw`) routes single-change PKs through a direct emit path, skipping window functions entirely. Impact: scan 100K/1% went from 46.5ms → 8.3ms (5.6×).

### Aggregate Dedup Elimination (Phase E)

Aggregates originally emitted D+I (delete+insert) row pairs per updated group, requiring a `DISTINCT ON` sort in the MERGE wrapper. A CASE-based single-row emit marks each group with the correct action directly (`'I'` for new groups, `'D'` for deleted groups, `'I'` for updated groups), making aggregate output inherently deduplicated. Eliminates the O(n log n) sort.

### Join Hash Simplification (Phase E)

Original join row IDs used nested hash calls: `pg_trickle_hash_multi(ARRAY[dl.__pgt_row_id::TEXT, pg_trickle_hash_multi(ARRAY[r."id"::TEXT])::TEXT])` — 2 Rust FFI crossings + 3 TEXT casts per row. Flattened to a single `pg_trickle_hash_multi(ARRAY[...])` call, reducing from 2 hash calls to 1 per row.

### Where Time is Spent (Current)

With pipeline overhead reduced to sub-2ms on cache hits, **MERGE execution dominates** all scenarios (69–93% of total refresh time). The remaining performance is determined by PostgreSQL's execution of the delta CTE chain and MERGE statement — not by the extension's orchestration layer.

| Phase | 100K/1% | 100K/10% |
|-------|---------|----------|
| Decision | <1ms | <2ms |
| Gen+Build | <1ms | <1ms |
| **MERGE** | **3–8ms** | **13–98ms** |
| Cleanup | <1ms | 3–5ms |

### Warm-Cycle Performance (INCR cycles 2+, excludes cold-start)

| Scenario | 10K/1% | 100K/1% | 100K/10% |
|----------|--------|---------|----------|
| scan | 6.9ms | 12.8ms | 107.8ms |
| filter | 7.8ms | 17.0ms | 54.3ms |
| join | 10.0ms | 29.9ms | 111.7ms |
| aggregate | 6.0ms | 8.3ms | 40.7ms |
| join_agg | 17.5ms | 18.5ms | 52.9ms |

### Full Evolution (scan 100K/1% — flagship scenario)

| Phase | INCR ms | INCR/FULL Ratio | Key Change |
|-------|---------|-----------------|------------|
| Baseline | 572.4 | 0.7× | Bug: manual refresh ignored mode |
| After P1+P2 | ~135 | 2.7× | Caching + JSONB fixes |
| After P3 | ~135 | 2.5× | SPI reduction (count→EXISTS) |
| After P5 (P7) | 46.5 | 7.0× | LATERAL VALUES, deferred cleanup |
| After P6 (Part 6) | **8.3** | **41.7×** | Scan fast path, join hash, aggregate dedup |

### Adaptive Threshold

The system automatically falls back from INCREMENTAL to FULL refresh when the change rate exceeds a configurable threshold (default: 15%). This prevents the pathological case where delta processing on large change sets is slower than full recomputation. The threshold can be tuned per stream table via the `pg_trickle.differential_max_change_ratio` GUC.
