# PLAN: Refresh Mode Diagnostics — `recommend_refresh_mode()` Advisory Function

**Date:** 2026-03-31
**Status:** Planning
**Scope:** New SQL-callable diagnostic function that analyzes stream table
characteristics and recommends the optimal refresh mode (FULL vs DIFFERENTIAL),
with explanation and confidence level.

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Current State](#2-current-state)
3. [Design](#3-design)
4. [Signal Analysis](#4-signal-analysis)
5. [Decision Algorithm](#5-decision-algorithm)
6. [Implementation Tasks](#6-implementation-tasks)
7. [Testing Strategy](#7-testing-strategy)
8. [Migration & Upgrade](#8-migration--upgrade)
9. [Future Extensions](#9-future-extensions)

---

## 1. Motivation

pg_trickle already has a capable AUTO mode that falls back from DIFFERENTIAL to
FULL under certain conditions (change ratio exceeds threshold, aggregate
saturation, TRUNCATE detection). However, users lack visibility into **why**
a particular mode was chosen and whether their per-table configuration is
optimal.

Common user questions that are currently hard to answer:

- "Is DIFFERENTIAL actually faster for my workload, or should I switch to FULL?"
- "What `auto_threshold` should I set for this table?"
- "Why did my refresh suddenly take 10× longer?" (silent AUTO fallback)
- "I have 50 stream tables — which ones would benefit from tuning?"

A **read-only diagnostic function** would codify the decision logic that
currently lives in operator intuition, surfacing it as structured,
explainable output.

### Non-Goals

- **Automatic mode switching:** The function is advisory only. It never
  changes configuration or behavior. Users remain authoritative.
- **Real-time adaptation:** This is an on-demand diagnostic, not a
  continuously-running optimizer.
- **Replacing AUTO mode:** AUTO mode's runtime fallback logic remains
  unchanged. This function provides a deeper, offline analysis.

---

## 2. Current State

### 2.1 Existing Infrastructure We Build On

| Component | Location | What it provides |
|-----------|----------|-----------------|
| AUTO fallback logic | `src/refresh.rs` L3500–3800 | Change-ratio threshold, aggregate saturation, TRUNCATE detection |
| `pgt_stream_tables` catalog | `src/catalog.rs` | `auto_threshold`, `last_full_ms`, `effective_refresh_mode`, `max_differential_joins`, `max_delta_fraction` |
| `pgt_refresh_history` | `src/catalog.rs` | Per-refresh execution log: rows inserted/deleted, duration, status, mode used |
| `st_refresh_stats()` | `src/monitor.rs` L477+ | Aggregated refresh statistics: total refreshes, avg duration, staleness |
| `check_cdc_health()` | `src/monitor.rs` | CDC trigger status, buffer row counts |
| Change buffer tables | `pgtrickle_changes.changes_<oid>` | Pending change counts per source |
| DVM parser | `src/dvm/parser.rs` | Query complexity analysis, operator tree |
| Planner hints | `src/refresh.rs` L622–745 | Delta-size-based hint injection (nestloop, work_mem, seqscan) |
| GUCs | `src/config.rs` | `differential_max_change_ratio`, `merge_seqscan_threshold`, etc. |

### 2.2 Gap Analysis

The existing AUTO mode makes its decision **at refresh time** based on a
single signal (change ratio vs threshold). The diagnostic function would
synthesize **multiple signals** into a richer recommendation, drawing on
historical data that AUTO mode ignores.

| Signal | Used by AUTO today | Available for diagnostics |
|--------|-------------------|--------------------------|
| Change ratio (current) | ✅ | ✅ |
| Aggregate saturation | ✅ | ✅ |
| TRUNCATE detection | ✅ | ✅ |
| Historical refresh durations | ❌ | ✅ (`pgt_refresh_history`) |
| Historical FULL vs DIFF comparison | ❌ | ✅ (when both modes have been used) |
| Query complexity score | ❌ | ✅ (DVM operator tree depth & breadth) |
| Index coverage on target | ❌ | ✅ (`pg_index` catalog) |
| Source → target size ratio | ❌ | ✅ (`pg_relation_size`) |
| P95 latency variance | ❌ | ✅ (`pgt_refresh_history`) |

---

## 3. Design

### 3.1 SQL Interface

```sql
-- Single table recommendation
SELECT * FROM pgtrickle.recommend_refresh_mode('my_schema.my_stream_table');

-- All stream tables
SELECT * FROM pgtrickle.recommend_refresh_mode();
```

### 3.2 Return Type

```sql
CREATE TYPE pgtrickle.refresh_recommendation AS (
    pgt_schema       text,
    pgt_name         text,
    current_mode     text,       -- configured: AUTO / FULL / DIFFERENTIAL
    effective_mode   text,       -- what actually ran last time
    recommended_mode text,       -- FULL / DIFFERENTIAL / KEEP (no change needed)
    confidence       text,       -- high / medium / low
    reason           text,       -- human-readable explanation
    signals          jsonb       -- structured signal data for programmatic consumption
);
```

### 3.3 Return Semantics

| `recommended_mode` | Meaning |
|--------------------|---------|
| `DIFFERENTIAL` | Switch to (or keep) DIFFERENTIAL mode |
| `FULL` | Switch to (or keep) FULL mode |
| `KEEP` | Current configuration is near-optimal; no change recommended |

| `confidence` | Meaning |
|--------------|---------|
| `high` | Strong signal — empirical timing data or extreme ratios |
| `medium` | Heuristic-based — reasonable but not empirically confirmed |
| `low` | Insufficient data — first refresh, no history, or conflicting signals |

### 3.4 Example Output

```
 pgt_schema | pgt_name     | current_mode | effective_mode | recommended_mode | confidence | reason                                                           | signals
------------+--------------+--------------+----------------+------------------+------------+------------------------------------------------------------------+---------
 public     | orders_daily | AUTO         | FULL           | DIFFERENTIAL     | high       | avg change ratio 0.03 well below threshold 0.15; DIFF 12ms vs FULL 340ms | {"change_ratio_avg": 0.03, ...}
 public     | user_summary | AUTO         | DIFFERENTIAL   | FULL             | high       | avg change ratio 0.61 exceeds threshold; DIFF 890ms vs FULL 340ms       | {"change_ratio_avg": 0.61, ...}
 public     | click_stream | DIFFERENTIAL | DIFFERENTIAL   | KEEP             | medium     | change ratio 0.08 within range; no FULL baseline for comparison          | {"change_ratio_avg": 0.08, ...}
 public     | new_table    | AUTO         | NULL           | KEEP             | low        | no refresh history available                                             | {}
```

---

## 4. Signal Analysis

### 4.1 Signals Collected

Each signal produces a score in `[-1.0, +1.0]` where negative favors FULL
and positive favors DIFFERENTIAL. Signals also carry a weight reflecting
reliability.

#### S1: Change Ratio (Current)

```
source: COUNT(*) on pgtrickle_changes.changes_<oid> / pg_class.reltuples
weight: 0.25
score:
    ratio < 0.05  → +1.0  (strongly favors DIFF)
    ratio < 0.15  → +0.5  (mildly favors DIFF)
    ratio < 0.30  → -0.3  (mildly favors FULL)
    ratio < 0.50  → -0.7  (favors FULL)
    ratio >= 0.50 → -1.0  (strongly favors FULL)
```

#### S2: Historical Change Ratio (Averaged)

```
source: AVG(rows_inserted + rows_deleted) / target reltuples
        from pgt_refresh_history (last 100 refreshes)
weight: 0.30 (higher than S1 — pattern is more reliable than a snapshot)
score:  same thresholds as S1
fallback: if <10 history rows, reduce weight to 0.10
```

#### S3: Empirical Timing Comparison

```
source: AVG(duration_ms) WHERE refresh_mode = 'DIFFERENTIAL'
        vs AVG(duration_ms) WHERE refresh_mode = 'FULL'
        from pgt_refresh_history (last 100 refreshes per mode)
weight: 0.35 (highest — empirical data trumps heuristics)
score:
    diff_avg < full_avg * 0.3  → +1.0  (DIFF >3× faster)
    diff_avg < full_avg * 0.7  → +0.5  (DIFF meaningfully faster)
    diff_avg < full_avg * 1.0  → +0.2  (DIFF slightly faster)
    diff_avg < full_avg * 1.5  → -0.5  (DIFF slower)
    diff_avg >= full_avg * 1.5 → -1.0  (DIFF much slower)
fallback: if either mode has <5 history rows, weight drops to 0.0
          (cannot compare without adequate samples of both modes)
```

#### S4: Query Complexity

```
source: DVM operator tree analysis (already parsed):
        - join_count: number of join nodes
        - aggregate_depth: nesting level of aggregates
        - has_window: window function presence
        - subquery_count: scalar/lateral subqueries
weight: 0.10
score:
    simple (scan/filter, 0 joins)       → +0.3  (DIFF is almost always better)
    moderate (1-2 joins, agg)           → +0.1  (slight DIFF advantage)
    complex (3+ joins or nested agg)    → -0.2  (DIFF overhead grows)
    very complex (5+ joins, subqueries) → -0.5  (DIFF may spill to disk)
```

#### S5: Target Table Size

```
source: pg_relation_size(target_oid) + pg_indexes_size(target_oid)
weight: 0.10
score:
    < 1 MB   → -0.3  (FULL is trivially fast on small tables)
    < 100 MB → +0.1  (neutral)
    < 1 GB   → +0.5  (DIFF avoids scanning large table)
    >= 1 GB  → +0.8  (DIFF strongly preferred)
```

#### S6: Index Coverage on Target

```
source: Check pg_index for indexes covering __pgt_row_id columns
        used by MERGE ... ON conditions
weight: 0.05
score:
    covering index exists → +0.2  (MERGE can use index lookup)
    no suitable index     → -0.2  (MERGE falls back to seqscan)
```

#### S7: Latency Variance

```
source: PERCENTILE_CONT(0.95) / PERCENTILE_CONT(0.50) on duration_ms
        from pgt_refresh_history (last 100 DIFFERENTIAL refreshes)
weight: 0.05
score:
    p95/p50 < 2.0  → +0.2  (stable latency)
    p95/p50 < 5.0  →  0.0  (acceptable variance)
    p95/p50 >= 5.0 → -0.3  (high variance, FULL may be more predictable)
fallback: if <20 history rows, weight drops to 0.0
```

### 4.2 Composite Score

```
composite = Σ(signal_score × signal_weight) / Σ(signal_weight)
```

Only signals with non-zero weight (after fallback adjustments) participate.
The composite score falls in `[-1.0, +1.0]`.

### 4.3 Confidence Derivation

```
total_weight = Σ(signal_weight)  -- after fallback adjustments
max_weight = Σ(signal_weight_original)  -- before adjustments

if total_weight >= 0.80 * max_weight  → confidence = 'high'
elif total_weight >= 0.50 * max_weight → confidence = 'medium'
else                                   → confidence = 'low'
```

Additionally, if any single signal with weight ≥ 0.25 has an absolute score
≥ 0.8, confidence is promoted to at least `medium` (a single strong signal
is enough to be directionally useful).

---

## 5. Decision Algorithm

### 5.1 Pseudocode

```
fn recommend(st: &StreamTableMeta) -> Recommendation:
    signals = collect_signals(st)
    composite = weighted_average(signals)
    confidence = derive_confidence(signals)
    
    if composite > +0.15:
        recommended = "DIFFERENTIAL"
    elif composite < -0.15:
        recommended = "FULL"
    else:
        recommended = "KEEP"  -- within dead zone, current config is fine
    
    // Don't recommend what they already have
    if recommended == st.effective_refresh_mode:
        recommended = "KEEP"
    
    reason = format_reason(signals, composite, recommended)
    signal_json = signals_to_jsonb(signals)
    
    return (recommended, confidence, reason, signal_json)
```

### 5.2 Reason Formatting

The `reason` text highlights the **top 2 signals** by `|score × weight|`,
formatted as a human-readable sentence:

```
"avg change ratio 0.03 well below threshold 0.15 (weight=0.30, score=+1.0);
 DIFF avg 12ms vs FULL avg 340ms (weight=0.35, score=+1.0)"
```

### 5.3 Signals JSONB Structure

```json
{
  "change_ratio_current": 0.03,
  "change_ratio_avg": 0.04,
  "diff_avg_ms": 12.3,
  "full_avg_ms": 340.1,
  "diff_p95_ms": 18.7,
  "target_size_bytes": 104857600,
  "join_count": 2,
  "has_covering_index": true,
  "history_rows_diff": 847,
  "history_rows_full": 92,
  "composite_score": 0.72,
  "signal_weights_used": 0.95,
  "signals": [
    {"name": "change_ratio_avg", "score": 1.0, "weight": 0.30},
    {"name": "empirical_timing", "score": 1.0, "weight": 0.35},
    {"name": "change_ratio_current", "score": 1.0, "weight": 0.25},
    {"name": "query_complexity", "score": 0.1, "weight": 0.10},
    {"name": "target_size", "score": 0.5, "weight": 0.10},
    {"name": "index_coverage", "score": 0.2, "weight": 0.05},
    {"name": "latency_variance", "score": 0.2, "weight": 0.05}
  ]
}
```

---

## 6. Implementation Tasks

### Phase 1: Core Function (D-1 through D-5)

#### D-1: Signal Collector Module

**File:** `src/diagnostics.rs` (new module)

Create a `SignalCollector` struct that gathers all signals for a given
stream table. Each signal is computed by a standalone function that takes
catalog/stats data as input (no SPI), enabling unit testing.

```rust
pub struct Signal {
    pub name: &'static str,
    pub score: f64,        // [-1.0, +1.0]
    pub weight: f64,       // [0.0, 1.0] after fallback adjustment
    pub base_weight: f64,  // original weight before adjustment
    pub detail: String,    // human-readable detail for this signal
}

pub struct Recommendation {
    pub recommended_mode: &'static str,  // "DIFFERENTIAL" | "FULL" | "KEEP"
    pub confidence: &'static str,        // "high" | "medium" | "low"
    pub reason: String,
    pub signals: Vec<Signal>,
    pub composite_score: f64,
}
```

Pure-logic functions (unit-testable without SPI):

- `score_change_ratio(ratio: f64) -> f64`
- `score_empirical_timing(diff_avg_ms: f64, full_avg_ms: f64) -> f64`
- `score_query_complexity(join_count: u32, agg_depth: u32, has_window: bool, subquery_count: u32) -> f64`
- `score_target_size(bytes: i64) -> f64`
- `score_index_coverage(has_covering: bool) -> f64`
- `score_latency_variance(p95: f64, p50: f64) -> f64`
- `compute_recommendation(signals: &[Signal]) -> Recommendation`

**Estimated complexity:** ~300 lines of Rust.

#### D-2: SPI Data Gathering

**File:** `src/diagnostics.rs`

SPI functions that collect the raw data for signal scoring:

- `gather_change_ratio(st: &StreamTableMeta) -> Result<f64>` — query current
  change buffer count + source `reltuples`
- `gather_history_stats(st_id: i64) -> Result<HistoryStats>` — query
  `pgt_refresh_history` for avg change ratio, avg duration per mode,
  percentile latencies
- `gather_query_complexity(st: &StreamTableMeta) -> Result<ComplexityStats>` —
  parse the view definition through the existing DVM parser to extract
  join count, aggregate depth, window presence, subquery count
- `gather_target_size(target_oid: Oid) -> Result<i64>` — call
  `pg_relation_size` + `pg_indexes_size`
- `gather_index_coverage(target_oid: Oid, row_id_cols: &[String]) -> Result<bool>` —
  check `pg_index` for indexes covering the `__pgt_row_id` columns

All SPI functions return `Result<T, PgTrickleError>` with graceful "not
found" handling (returning `None` or neutral defaults rather than erroring).

**Estimated complexity:** ~200 lines of Rust.

#### D-3: SQL Function Registration

**File:** `src/api.rs`

Register two `#[pg_extern]` functions:

```rust
#[pg_extern(schema = "pgtrickle")]
fn recommend_refresh_mode(
    st_name: default!(Option<String>, "NULL"),
) -> TableIterator<'static, (
    name!(pgt_schema, String),
    name!(pgt_name, String),
    name!(current_mode, String),
    name!(effective_mode, Option<String>),
    name!(recommended_mode, String),
    name!(confidence, String),
    name!(reason, String),
    name!(signals, pgrx::JsonB),
)> { ... }
```

When `st_name` is `NULL`, iterate over all active stream tables. When
provided, resolve the name (schema-qualified or search-path) and return a
single row.

**Estimated complexity:** ~80 lines of Rust.

#### D-4: Unit Tests for Signal Scoring

**File:** `src/diagnostics.rs` (`#[cfg(test)]` module)

Test each scoring function with boundary values:

- `test_score_change_ratio_boundaries` — 0.0, 0.05, 0.15, 0.30, 0.50, 1.0
- `test_score_empirical_timing_boundaries` — ratio 0.1, 0.3, 0.7, 1.0, 1.5, 3.0
- `test_score_query_complexity_levels` — simple, moderate, complex, very complex
- `test_score_target_size_range` — 100KB, 10MB, 500MB, 2GB
- `test_composite_score_all_favor_diff` — all signals positive
- `test_composite_score_all_favor_full` — all signals negative
- `test_composite_score_mixed_signals` — conflicting signals, check dead zone
- `test_confidence_derivation` — full data, partial data, minimal data
- `test_recommendation_keep_when_in_dead_zone` — composite near zero
- `test_recommendation_keep_when_already_optimal` — recommended == effective

**Estimated complexity:** ~200 lines of Rust.

#### D-5: Integration Tests

**File:** `tests/e2e_diagnostics_tests.rs`

E2E tests using the extension in a Testcontainers PostgreSQL instance:

- `test_recommend_refresh_mode_no_stream_tables` — empty result set
- `test_recommend_refresh_mode_new_table_low_confidence` — freshly created
  ST with no history returns `confidence = 'low'`
- `test_recommend_refresh_mode_after_refreshes` — create ST, run 20+
  refreshes with varying change loads, verify recommendation is reasonable
- `test_recommend_refresh_mode_single_table` — pass specific table name,
  verify single-row result
- `test_recommend_refresh_mode_signals_jsonb` — verify JSONB structure contains
  expected keys
- `test_recommend_refresh_mode_high_change_ratio` — insert changes >50% of
  source, verify FULL recommendation
- `test_recommend_refresh_mode_schema_qualified` — `'myschema.mytable'` works

**Estimated complexity:** ~250 lines of Rust.

### Phase 2: Companion Monitoring View (D-6)

#### D-6: `refresh_efficiency` View

**File:** `src/api.rs`

A convenience SQL view (or set-returning function) that exposes operational
refresh efficiency metrics without the recommendation logic:

```sql
SELECT * FROM pgtrickle.refresh_efficiency;

 pgt_schema | pgt_name     | refresh_mode | effective_mode | total_refreshes | diff_count | full_count | avg_diff_ms | avg_full_ms | avg_change_ratio | diff_speedup | last_refresh_at
------------+--------------+--------------+----------------+-----------------+------------+------------+-------------+-------------+------------------+--------------+----------------
 public     | orders_daily | AUTO         | DIFFERENTIAL   | 1847            | 1755       | 92         | 12.3        | 340.1       | 0.03             | 27.6x        | 2026-03-31 ...
```

This reuses the same SPI queries as D-2 but presents raw data rather than
a recommendation. Users who want to make their own judgment get the
numbers; users who want advice call `recommend_refresh_mode()`.

**Estimated complexity:** ~100 lines of Rust.

### Phase 3: Documentation (D-7)

#### D-7: SQL Reference & Configuration Docs

- Add `recommend_refresh_mode()` to `docs/SQL_REFERENCE.md`
- Add `refresh_efficiency` view to `docs/SQL_REFERENCE.md`
- Add a "Diagnostics & Tuning" section to `docs/CONFIGURATION.md` explaining
  how to use the function output to tune `auto_threshold`,
  `max_differential_joins`, and `max_delta_fraction`
- Add a tutorial entry `docs/tutorials/tuning-refresh-mode.md`

---

## 7. Testing Strategy

### 7.1 Tier Mapping

| Task | Test Tier | Runner |
|------|-----------|--------|
| D-4: Signal scoring unit tests | Unit | `just test-unit` |
| D-5: E2E function tests | Light E2E | `just test-light-e2e` |
| D-6: View tests | Light E2E | `just test-light-e2e` |

### 7.2 Property-Based Tests (Optional Enhancement)

For D-4, consider proptest strategies:

- `change_ratio in 0.0..1.0` → score is monotonically non-increasing
- `diff_avg, full_avg in 1.0..10000.0` → score sign matches expected direction
- `composite of N signals` → always in `[-1.0, +1.0]`

### 7.3 Idempotency

`recommend_refresh_mode()` must be fully read-only. Test that calling it
twice returns identical results (given no intervening refreshes).

---

## 8. Migration & Upgrade

### 8.1 New SQL Objects

The upgrade migration must register:

- `pgtrickle.recommend_refresh_mode(text)` — set-returning function
- `pgtrickle.refresh_efficiency` — view (or set-returning function)

### 8.2 No Catalog Changes

This feature adds no new catalog tables or columns. It reads existing
`pgt_stream_tables`, `pgt_refresh_history`, `pgt_dependencies`, and
PostgreSQL system catalogs. The upgrade migration is purely additive.

### 8.3 Upgrade Script

Add entries to the appropriate `sql/pg_trickle--X.Y.Z--X.Y.W.sql` migration.
Verify with `scripts/check_upgrade_completeness.sh`.

---

## 9. Future Extensions

### 9.1 `EXPLAIN REFRESH`

A natural follow-on that goes deeper: show the delta SQL plan, estimated costs,
and which planner hints would be applied, without executing the refresh.

### 9.2 Automated Threshold Tuning

If `recommend_refresh_mode()` surfaces consistent recommendations over time,
a future `pgtrickle.auto_tune()` function could apply them — but only with
explicit user invocation, never silently.

### 9.3 Grafana Dashboard Integration

The `signals` JSONB output is designed for programmatic consumption. A Grafana
dashboard panel could poll `recommend_refresh_mode()` on a schedule and
surface tables that would benefit from configuration changes.

### 9.4 Historical Trend Analysis

Extend `pgt_refresh_history` with a `change_ratio` column (populated during
refresh) to enable time-series analysis of change patterns. This would improve
S2 signal accuracy by capturing the actual ratio at each refresh rather than
computing an estimate from row counts after the fact.

---

## Appendix A: Dependency Graph

```
D-1 (Signal Collector)
 ├── D-2 (SPI Gathering) ── depends on D-1 types
 │    └── D-3 (SQL Function) ── depends on D-1 + D-2
 │         └── D-5 (Integration Tests)
 ├── D-4 (Unit Tests) ── depends on D-1 only
 └── D-6 (Efficiency View) ── depends on D-2 queries
      └── D-7 (Documentation) ── depends on D-3 + D-6
```

## Appendix B: Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Signal weights produce poor recommendations | Medium | Low (advisory only) | Extensive unit tests; tunable weights via future GUCs |
| SPI overhead for gathering signals is too high | Low | Low | All queries use existing catalog indexes; no sequential scans |
| `pgt_refresh_history` has insufficient data | Medium | Low | Confidence drops to `low`; function remains useful with degraded accuracy |
| Query complexity scoring is too coarse | Medium | Low | Weight is only 0.10; refine in future iterations |
| Users ignore the function | Medium | N/A | Zero cost if unused; no runtime overhead |
