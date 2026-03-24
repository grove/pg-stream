# pg_trickle — Overall Project Status & Performance Report

**Date:** 2026-03-24  
**Version:** 0.10.0 (updated with deep gap analysis)  
**Author:** Deep analysis of architecture, configuration, code paths, benchmarks, and plans

---

## Executive Summary

pg_trickle is a PostgreSQL 18 extension implementing **streaming tables with
incremental view maintenance (IVM)** based on differential dataflow (DBSP). At
v0.10.0, the project has reached a high level of maturity with 21 DVM
operators, hybrid CDC, parallel refresh, and broad SQL coverage including
recursive CTEs, window functions, LATERAL joins, and set operations.

**Key findings:**

| Dimension | Status | Rating |
|-----------|--------|--------|
| Correctness | 36 edge cases catalogued; all P0 resolved; EC-01 ≥3-scan boundary remains | ★★★★☆ |
| Performance | 7–42× speedup at 1% change rate; MERGE is 70–97% of time | ★★★★☆ |
| Defaults | Conservative and safe; advanced features opt-in | ★★★★★ |
| Scalability | Parallel refresh implemented but off by default | ★★★☆☆ |
| Safety | 51% unsafe reduction; 1 production panic site; 65 unwrap calls in core modules | ★★★☆☆ |
| Ergonomics | Good basics; silent AUTO mode downgrades; no explain functions | ★★★☆☆ |
| Observability | NOTIFY alerts, refresh stats, health checks, dependency tree | ★★★★☆ |
| SQL Coverage | 21 operators, 60+ aggregate functions; VALUES/TABLESAMPLE unsupported | ★★★★☆ |
| DVM Engine | 19.7K-line parser; 25 group-rescan aggregates; no offline SQL validation | ★★★☆☆ |
| Documentation | Good reference; missing support matrix, interaction guides, patterns | ★★★☆☆ |
| Deployment | PgBouncer, CNPG, backup/restore, dbt integration | ★★★★☆ |

**Overall assessment:** The project is production-ready for typical analytical
workloads. The primary performance bottleneck is PostgreSQL's own MERGE
execution (70–97% of refresh time), meaning the Rust-side pipeline is highly
optimized. The main opportunities for improvement are in scaling to larger
deployments (100+ stream tables), reducing CDC write-side overhead,
unlocking the parallel refresh feature by default, and addressing the
ergonomics/observability gaps identified in this deep analysis (§12–§17).

---

## Table of Contents

1. [Architecture & Design Quality](#1-architecture--design-quality)
2. [Default Configuration Analysis](#2-default-configuration-analysis)
3. [Performance Status](#3-performance-status)
4. [CDC Pipeline Analysis](#4-cdc-pipeline-analysis)
5. [DVM Engine Analysis](#5-dvm-engine-analysis)
6. [Scheduler & Coordination](#6-scheduler--coordination)
7. [Safety & Error Handling](#7-safety--error-handling)
8. [Scalability Assessment](#8-scalability-assessment)
9. [End-to-End Latency Analysis](#9-end-to-end-latency-analysis)
10. [Recommendations for Future Releases](#10-recommendations-for-future-releases)
11. [Summary & Prioritized Roadmap](#11-summary--prioritized-roadmap)
12. [Deep Gap Analysis — DVM Correctness & SQL Coverage](#12-deep-gap-analysis--dvm-correctness--sql-coverage)
13. [Deep Gap Analysis — Safety & Code Quality](#13-deep-gap-analysis--safety--code-quality)
14. [Deep Gap Analysis — Performance Opportunities](#14-deep-gap-analysis--performance-opportunities)
15. [Deep Gap Analysis — Ergonomics & API Design](#15-deep-gap-analysis--ergonomics--api-design)
16. [Deep Gap Analysis — Documentation & Onboarding](#16-deep-gap-analysis--documentation--onboarding)
17. [Deep Gap Analysis — Testing & Verification](#17-deep-gap-analysis--testing--verification)

---

## 1. Architecture & Design Quality

### 1.1 Component Overview

The system is structured into well-separated layers:

```
User SQL API (api.rs)
    ↓
Catalog (catalog.rs) ←→ DDL Hooks (hooks.rs)
    ↓
CDC Layer (cdc.rs / wal_decoder.rs)
    ↓ Change buffers (pgtrickle_changes schema)
DVM Engine (dvm/) — Parser → OpTree → Delta SQL
    ↓
Refresh Engine (refresh.rs) — FULL / DIFFERENTIAL / IMMEDIATE
    ↓
Scheduler (scheduler.rs) ←→ DAG (dag.rs) ←→ Shared Memory (shmem.rs)
    ↓
Monitoring (monitor.rs) — Stats, alerts, health checks
```

### 1.2 Design Strengths

- **Single-binary extension:** No external dependencies beyond PostgreSQL
  itself. No message queues, no separate services.
- **Theoretical foundation:** Grounded in DBSP (Budiu et al. 2023), giving
  formal correctness guarantees for differential computation.
- **Layered optimization:** Each layer has independent optimization passes
  (CDC column pruning → DVM predicate pushdown → MERGE planner hints →
  prepared statements).
- **Adaptive behavior:** Automatic fallback from DIFFERENTIAL to FULL when
  change ratio exceeds threshold; auto-backoff when refresh outpaces schedule.
- **Crash safety:** Frontier-based versioning ensures no data loss; orphaned
  RUNNING records auto-recovered on scheduler restart.

### 1.3 Design Concerns

- **Thread-local caching:** Delta SQL templates and MERGE plans cached per
  backend session via `thread_local!`. Cross-session invalidation uses a shared
  atomic counter (`CACHE_GENERATION`), but cache warm-up cost hits every new
  connection.
- **Shared memory ring buffer:** 32-slot invalidation ring for DAG changes.
  Overflow triggers full O(V+E) DAG rebuild. Under heavy DDL (>32 CREATE/ALTER
  in one tick), this is a cliff.
- **SPI coupling:** All catalog access and MERGE execution happen through SPI,
  which holds lightweight locks and may conflict with concurrent DDL. SPI
  blocks are kept short per coding guidelines, but the total SPI time per
  refresh cycle is significant.

---

## 2. Default Configuration Analysis

### 2.1 Default Values — Safety Assessment

The defaults prioritize **safety and predictability** over maximum throughput:

| GUC | Default | Assessment |
|-----|---------|------------|
| `enabled` | `true` | ✅ Expected |
| `cdc_mode` | `'auto'` | ✅ Safe — starts with triggers, upgrades to WAL if available |
| `cdc_trigger_mode` | `'statement'` | ✅ Optimal — 50–80% less write overhead than row-level |
| `scheduler_interval_ms` | `1000` | ✅ Balanced — 1s wake cycle; responsive without busy-looping |
| `min_schedule_seconds` | `1` | ✅ After ergonomics fix (was 60) |
| `default_schedule_seconds` | `1` | ✅ Fast default for isolated CALCULATED STs |
| `differential_max_change_ratio` | `0.15` | ✅ Conservative — falls back to FULL at 15% change rate |
| `cleanup_use_truncate` | `true` | ✅ O(1) cleanup vs O(n) DELETE |
| `use_prepared_statements` | `true` | ✅ 1–2ms savings per refresh |
| `merge_planner_hints` | `true` | ✅ Dynamic optimizer hints improve join strategies |
| `merge_work_mem_mb` | `64` | ✅ Reasonable for MERGE sorting |
| `tick_watermark_enabled` | `true` | ✅ Critical for cross-source consistency |
| `compact_threshold` | `100,000` | ✅ Triggers compaction before buffers grow large |

### 2.2 Defaults That Limit Performance (by design)

| GUC | Default | Why Off | Impact if Enabled |
|-----|---------|---------|-------------------|
| `parallel_refresh_mode` | `'off'` | Safety — new feature, needs explicit opt-in | **2.5–5× throughput** for independent ST graphs |
| `auto_backoff` | `false` | Predictability — users expect fixed schedules | Prevents scheduler overload when refresh > schedule |
| `tiered_scheduling` | `false` | Simplicity — equal freshness for all STs | Reduces refresh load for rarely-queried STs |
| `buffer_partitioning` | `'off'` | Overhead — partition management adds CDC cost | Faster cleanup for high-throughput sources |
| `allow_circular` | `false` | Correctness — cyclic STs have convergence risks | Enables feedback-loop patterns |
| `block_source_ddl` | `false` | Ergonomics — blocking DDL is surprising | Prevents schema drift breaking STs silently |

### 2.3 Default Configuration Verdict

**The defaults are well-chosen.** They provide:
- ✅ **Predictable behavior** — no surprises for new users
- ✅ **Safe operations** — conservative fallback thresholds
- ✅ **Good baseline performance** — statement-level triggers, prepared
  statements, planner hints all enabled by default
- ⚠️ **Suboptimal for large deployments** — parallel refresh off,
  tiered scheduling off, no auto-backoff

The design follows the principle of "safe by default, fast by configuration."

---

## 3. Performance Status

### 3.1 Current Benchmark Results

**Headline numbers at 100K rows, 1% change rate (typical production):**

| Scenario | FULL (ms) | INCREMENTAL (ms) | Speedup |
|----------|-----------|-------------------|---------|
| scan | 300 | 4.6 | **65.9×** |
| filter | 134 | 3.4 | **39.5×** |
| aggregate | 17 | 2.5 | **6.8×** |
| join | 294 | 18.0 | **16.3×** |
| join_agg | 28 | 14.6 | **1.9×** |

**Zero-change latency:** < 10ms (target achieved via `EXISTS(<buffer>)` short-circuit)

### 3.2 Performance Evolution

The project has gone through 9 optimization phases:

| Phase | Key Optimization | Impact |
|-------|-----------------|--------|
| Baseline | — | INCREMENTAL slower than FULL for some cases |
| P1 | JSONB fix, drop unused columns, LSN bounds | 2.7× improvement |
| P2 | Delta SQL caching, MERGE template caching | +10–20ms/refresh |
| P3 | LATERAL VALUES for aggregates, PK resolution | Correctness + speed |
| P4 | No-op short-circuit, prepared statements | **41.7× peak** |
| P5 | Deferred cleanup, warm-cycle tuning | Stability |
| P6 | Statement-level triggers | 50–80% write reduction |
| P7 | Parallel refresh infrastructure | 2.5–5× throughput (opt-in) |
| P8–P9 | Algebraic aggregates, covering indexes, compaction | 20–50% latency reduction |

### 3.3 Where Time Is Spent

**Critical path breakdown (100K rows, 1% change rate):**

| Component | Time | % of Total |
|-----------|------|------------|
| Decision phase | < 1ms | < 1% |
| Delta SQL generation (Rust) | < 1ms | < 1% |
| MERGE plan + execution (PG) | 3–15ms | **70–97%** |
| Cleanup (deferred) | < 7.7ms | 2–20% |
| SPI overhead | < 1ms | < 5% |

**The Rust-side pipeline is essentially negligible.** Further Rust
optimization yields diminishing returns. The bottleneck is PostgreSQL's MERGE
executor — plan compilation, index lookups, heap writes, and WAL generation.

### 3.4 Pure Rust Micro-benchmarks

From Criterion.rs (benches/):

| Operation | Time |
|-----------|------|
| `quote_ident` (simple) | 93.9 ns |
| `col_list` (10 cols) | 1.37 µs |
| `prefixed_col_list` (20 cols) | 4.00 µs |
| `expr_to_sql` (column ref) | 26.3 ns |
| `lsn_gt` comparison | 78.9 ns |
| `frontier_json` serialize (20) | 3.76 µs |
| `dag_build_linear_100` | 62.5 µs |
| `xxh64` (4096 bytes) | 384.1 ns |
| `diff_scan` (20 cols) | 47.7 µs |
| `diff_inner_join` | 30.7 µs |
| `diff_full_pipeline` | 54.2 µs |

All operations are sub-millisecond. The delta SQL generation pipeline
(`diff_full_pipeline` at 54.2 µs) is 3–4 orders of magnitude faster than MERGE
execution.

### 3.5 Scaling Characteristics

| Change Rate | 10K rows | 100K rows | Trend |
|-------------|----------|-----------|-------|
| **1%** | 1.5–5× | 5–66× | Better at scale |
| **10%** | 1.4–2.2× | 1.8–3.3× | Moderate benefit |
| **50%** | 0.6–1.3× | 0.6–1.1× | FULL is better; adaptive fallback kicks in |

INCREMENTAL scales super-linearly with table size (because delta stays small
while FULL grows linearly). The adaptive fallback threshold at 15% prevents
INCREMENTAL from being used when it would be slower.

---

## 4. CDC Pipeline Analysis

### 4.1 Trigger-Based CDC (Default)

**Architecture:** Row-level or statement-level AFTER triggers capture DML into
typed buffer tables (`pgtrickle_changes.changes_<oid>`).

**Statement-level triggers** (default since v0.4.0):
- Single trigger invocation per DML statement (not per row)
- Uses `REFERENCING NEW TABLE AS __pgt_new` transition tables
- **50–80% less write-side overhead** than row-level for bulk DML
- Slightly higher per-invocation planning cost (amortized over batch size)

**Buffer table schema:**
- `lsn` — WAL position for ordering
- `action` — I/U/D/T (insert/update/delete/truncate)
- `pk_hash` — xxHash64 of primary key columns
- `changed_cols` — Bitmask for UPDATE (which columns changed)
- `new_*/old_*` — Per-column typed values (no JSONB serialization)

### 4.2 CDC Optimizations Implemented

| Optimization | Version | Impact |
|-------------|---------|--------|
| Statement-level triggers | v0.4.0 | 50–80% write overhead reduction |
| Typed buffer columns (no JSONB) | v0.1.0 | Eliminates serialization cost |
| Single covering index | v0.10.0 | 20% trigger overhead reduction |
| Changed columns bitmask | v0.4.0 | Skip unchanged columns in UPDATE |
| Selective CDC capture | v0.9.0 | 50–80% reduction for wide tables (only capture referenced columns) |
| Change buffer compaction | v0.10.0 | 50–90% delta reduction (cancel INSERT→DELETE pairs) |
| Partitioned buffers | v0.6.0 | Faster cleanup via DETACH+DROP |
| RLS bypass on buffers | v0.5.0 | No policy overhead on internal tables |

### 4.3 CDC Write-Side Overhead (Estimated)

Based on trigger overhead analysis planning:

| Table Width | DML Type | Est. Overhead/Row | Write Slowdown |
|-------------|----------|-------------------|----------------|
| Narrow (3 cols) | INSERT | 1–3 µs | 1.5–2.5× |
| Narrow (3 cols) | UPDATE | 2–5 µs | 2.0–3.0× |
| Medium (8 cols) | UPDATE | 3–8 µs | 2.5–4.0× |
| Wide (20 cols) | UPDATE | 5–15 µs | 3.0–5.0× |

The write-side overhead is moderate and acceptable for OLTP workloads.
Statement-level triggers reduce this significantly for batch operations.

### 4.4 WAL-Based CDC (Optional)

Available when `wal_level = 'logical'`. Uses `pgoutput` logical replication:

- **Transition lifecycle:** TRIGGER → TRANSITIONING → WAL (with automatic
  fallback)
- **Text-based parsing:** `pgoutput` messages parsed positionally (G2.3)
- **Progressive timeout:** 1×/2×/3× timeout with LOG warnings before abort

**Current state:** Functional but secondary. Trigger-based CDC is preferred
for single-transaction atomicity and simpler operations.

### 4.5 CDC Bottlenecks & Opportunities

| Bottleneck | Severity | Mitigation |
|------------|----------|------------|
| PL/pgSQL trigger overhead | Medium | Statement-level triggers (implemented) |
| WAL writes for buffer tables | Medium | UNLOGGED buffers (planned, behind GUC) |
| pgoutput text parsing (WAL mode) | Low | Positional parsing (implemented) |
| Column-level capture for `SELECT *` | Low | Falls back to full capture (correct) |
| Buffer index maintenance on INSERT | Low | Single covering index (implemented) |

---

## 5. DVM Engine Analysis

### 5.1 Operator Coverage

21 operators covering virtually all analytical SQL patterns:

| Category | Operators | Status |
|----------|-----------|--------|
| Basic | Scan, Filter, Project | ✅ Optimized |
| Joins | Inner, Left, Right, Full, Semi, Anti | ✅ Correct (B3-2 weight aggregation) |
| Aggregates | GROUP BY with 60+ aggregate functions | ✅ Algebraic maintenance for AVG/STDDEV/VAR |
| Set Operations | UNION ALL, INTERSECT, EXCEPT | ✅ |
| Advanced | Window, Distinct, Subquery | ✅ |
| CTE | CteScan, RecursiveCte, RecursiveSelfRef | ✅ Semi-naive evaluation |
| Lateral | LateralFunction, LateralSubquery | ✅ |
| Scalar Subquery | ScalarSubquery | ✅ Decorrelation rewrite |

### 5.2 Query Rewriting Pipeline

Six normalization passes before DVM differentiation:

1. **View inlining** — Fixpoint closure (inline views, re-inline if result
   contains more views)
2. **DISTINCT ON → ROW_NUMBER OVER** — Canonical form
3. **GROUPING SETS/CUBE/ROLLUP → UNION ALL** — Explosion guard
   (`max_grouping_set_branches` GUC, default 64)
4. **Scalar subquery decorrelation** — Rewrites correlated subqueries
5. **De Morgan normalization** — Multi-pass sublink rewriting
6. **Nested window expression lifting** — Canonical window form

### 5.3 DVM Performance Characteristics

- **Delta SQL generation:** 47–54 µs for typical queries (negligible)
- **Template caching:** Eliminates re-parsing after first execution (~45ms
  saved per refresh cycle)
- **CTE delta memoization:** Each CTE reference reuses the same delta
  subquery (no redundant computation)
- **Predicate pushdown:** Filter conditions pushed into change buffer scans
  (reduces data volume early)

### 5.4 DVM Correctness Measures

- **TPC-H validation:** All 22 queries verified against known-correct results
- **Property-based testing:** proptest regressions tracked
- **Weight aggregation:** Z-set algebra for diamond flows (replaces incorrect
  DISTINCT ON)
- **Semi-naive evaluation:** Recursive CTEs use incremental fixpoint
- **Delete-and-Rederive:** Correct deletion for recursive CTEs

---

## 6. Scheduler & Coordination

### 6.1 Architecture

Two-tier background worker model:

1. **Launcher** (one per cluster) — discovers databases, spawns schedulers
2. **Per-database scheduler** — reads DAG, coordinates refreshes, manages
   retries

### 6.2 Refresh Coordination

The scheduler executes a topological refresh cycle:

1. Build/update DAG from catalog
2. Detect consistency groups (diamond dependencies → atomic refresh)
3. For each group in topological order:
   - Check schedule staleness
   - Determine refresh action (FULL/DIFFERENTIAL/NO_DATA)
   - Execute with retry backoff (max 3 consecutive errors → auto-suspend)
4. Record history, emit NOTIFY alerts

### 6.3 Parallel Refresh (opt-in)

Implemented in v0.4.0 via dynamic background workers:

- **Coordinator** dispatches independent execution units to workers
- **Worker tokens:** CAS-based allocation with cluster-wide budget
  (`max_dynamic_refresh_workers` default 4)
- **Wave reset:** After all in-flight workers complete and any succeeded,
  re-evaluate upstream counts for cascading refreshes
- **Diamond protection:** Atomic groups always execute serially within a
  consistency boundary

### 6.4 Advanced Scheduling Features

| Feature | Default | Description |
|---------|---------|-------------|
| Tiered scheduling | Off | HOT (1×), WARM (2×), COLD (4×), FROZEN (skip) |
| Auto-backoff | Off | Doubles schedule when refresh > schedule interval |
| Adaptive threshold | On | Auto-tunes FULL/DIFF cutoff from historical data |
| Cyclic SCC iteration | Off | Fixed-point convergence for circular dependencies |
| Crash recovery | On | Auto-marks orphaned RUNNING records as FAILED |
| Tick watermark | On | Cross-source snapshot consistency |

### 6.5 Scheduler Bottlenecks

| Bottleneck | Impact | Current Mitigation |
|------------|--------|-------------------|
| Serialized refresh (default) | Only 1 ST refreshed at a time | Parallel mode available (opt-in) |
| DAG rebuild on ring overflow | O(V+E) with >32 concurrent DDL | Incremental rebuild fallback |
| Per-ST catalog lock probes | `FOR UPDATE SKIP LOCKED` per ST | Capped by concurrent_refreshes GUC |
| History queries for cost model | Last 10+5 records per ST per cycle | SPI queries; infrequent |

---

## 7. Safety & Error Handling

### 7.1 Unsafe Code

- **Before reduction:** 1,309 unsafe blocks
- **After reduction (v0.7.0):** 641 unsafe blocks (**51% reduction**)
- **Approach:** Six well-documented safe abstractions (`pg_cstr_to_str`,
  `pg_list`, `cast_node!`, `parse_query`, `SubTransaction`, function
  conversions)
- **All remaining `unsafe` blocks** have `// SAFETY:` comments per project
  guidelines

### 7.2 Error Handling

- **PgTrickleError enum** with classified variants (user, schema, system,
  internal)
- **No `unwrap()` or `panic!()`** in code reachable from SQL
- **SQLSTATE-based retry classification** (F29) — identifies retriable errors
  (lock timeouts, serialization failures) vs permanent errors
- **Exponential backoff with jitter** for retries
- **Max consecutive errors** (default 3) before auto-suspend with NOTIFY alert

### 7.3 Concurrency Safety

- **PgLwLock** for shared state (short hold times)
- **PgAtomic** (CAS) for counters (worker tokens, cache generation, DAG version)
- **Advisory locks** for non-blocking compaction attempts
- **SAVEPOINT** for atomic consistency group refresh (rollback on any failure)
- **FOR UPDATE SKIP LOCKED** for concurrent ST refresh coordination

### 7.4 Data Safety

- **Frontier-based versioning:** Stream tables represent query result at a
  consistent past timestamp (Delayed View Semantics)
- **Cross-source tick watermark:** All refreshes in a tick capped to a single
  WAL LSN
- **Crash recovery:** Orphaned RUNNING records auto-detected and marked FAILED
- **DML guard trigger:** Prevents direct INSERT/UPDATE/DELETE on stream tables
- **Schema change detection:** DDL event triggers detect upstream changes and
  mark STs for reinitialization

---

## 8. Scalability Assessment

### 8.1 Current Scalability Profile

| Dimension | Status | Limit |
|-----------|--------|-------|
| Stream table count | Good | Tested with ~100 STs; DAG operations O(V+E) |
| Source table size | Good | 100K–1M rows benchmarked; MERGE scales with delta size |
| Change rate | Good | Adaptive fallback prevents degradation at high rates |
| Column count | Good | Selective CDC + bitmask up to 63 columns |
| Query complexity | Good | 21 operators; TPC-H 22 queries validated |
| Concurrent databases | Good | Per-database scheduler isolation |
| Concurrent writers | Moderate | Statement-level triggers reduce contention |
| Parallel refresh | Opt-in | 4 workers default; proven 2.5–5× throughput |

### 8.2 Scalability Limits

| Constraint | Current | Concern |
|------------|---------|---------|
| Invalidation ring | 32 slots | >32 concurrent DDL → full DAG rebuild |
| Changed columns bitmask | 63 columns (BIGINT) | >63 columns loses per-column tracking |
| TopK LIMIT | 1,000 default | Large LIMIT values degrade IMMEDIATE mode |
| Recursive CTE depth | 100 default | Deep recursion may not converge |
| Fixpoint iterations | 100 default | Cyclic STs may not converge |
| MERGE executor | PG-internal | Cannot optimize beyond planner hints |

### 8.3 Deployment Compatibility

| Platform | Status | Notes |
|----------|--------|-------|
| PostgreSQL 18 | ✅ | Primary target |
| PgBouncer | ✅ v0.10.0 | Pooler compatibility mode |
| CloudNativePG | ✅ v0.1.1 | Extension-only Docker images |
| Backup/restore | ✅ v0.8.0 | Auto-reconnect infrastructure |
| dbt | ✅ v0.4.0 | Materialization + health macros |
| Logical replication | ⚠️ | WAL CDC mode; not for read replicas |

---

## 9. End-to-End Latency Analysis

### 9.1 Latency Components

For a single stream table refresh cycle:

```
Source DML committed
    ↓ [CDC trigger overhead: 1–15 µs/row]
Change buffer populated
    ↓ [Scheduler wake: 0–1000ms (scheduler_interval_ms)]
Scheduler tick
    ↓ [Schedule check: < 1ms]
    ↓ [Decision phase: < 1ms]
    ↓ [Delta SQL generation: < 0.1ms (cached)]
Refresh execution
    ↓ [MERGE plan + execute: 3–15ms at 1% change rate]
    ↓ [History logging: < 1ms]
Stream table updated
    ↓ [Deferred cleanup: runs next cycle]
```

### 9.2 End-to-End Latency Budget

| Component | Best Case | Typical | Worst Case |
|-----------|-----------|---------|------------|
| CDC trigger | < 1ms (bulk) | 1–5ms | 15ms (wide, row-level) |
| Scheduler wake delay | 0ms | 500ms | 1000ms |
| Schedule check | < 1ms | < 1ms | < 1ms |
| Delta SQL (cached) | < 0.1ms | < 0.1ms | 45ms (cold cache) |
| MERGE execution | 3ms | 10ms | 300ms+ (large delta) |
| **Total** | **~4ms** | **~515ms** | **>1300ms** |

**Dominant latency factor:** Scheduler wake interval contributes the most to
typical end-to-end latency. For IMMEDIATE mode (IVM), the latency drops to
the MERGE execution time alone (~3–15ms), since changes are applied
synchronously within the same transaction.

### 9.3 Throughput Analysis

| Metric | Value | Condition |
|--------|-------|-----------|
| Refreshes/second (serial) | ~50–200 | 1 ST, 1% change rate |
| Refreshes/second (parallel) | ~200–1000 | 4 workers, independent STs |
| Zero-change throughput | ~500+ | `EXISTS` short-circuit, < 2ms |
| Source write throughput impact | 1.5–5× slower | Depends on table width and trigger mode |
| Max STs per database | ~100+ tested | DAG rebuild < 100ms |

---

## 10. Recommendations for Future Releases

### 10.1 High Priority — Performance & Throughput

#### R1: Enable Parallel Refresh by Default (v0.11)

**Current:** `parallel_refresh_mode = 'off'`  
**Recommendation:** Change default to `'on'`  
**Impact:** 2.5–5× throughput improvement for deployments with independent STs  
**Risk:** Low — the feature is implemented and tested since v0.4.0; six
releases of stabilization  
**Prerequisite:** Validate with TPC-H and dbt integration tests under parallel
mode

#### R2: UNLOGGED Change Buffers (v0.11–v0.12)

**Current:** Change buffers are standard WAL-logged tables  
**Recommendation:** Add GUC `unlogged_change_buffers` (default `'auto'`)  
**Impact:** ~30% reduction in CDC write-side WAL volume  
**Risk:** Medium — UNLOGGED tables are lost on crash. Requires frontier reset
logic to detect lost buffers and trigger full refresh. This is acceptable
because change buffers are transient by nature.  
**Mitigation:** On recovery, detect missing UNLOGGED tables and force FULL
refresh for affected STs

#### R3: Reduce Scheduler Wake Interval for Latency-Sensitive Workloads (v0.11)

**Current:** `scheduler_interval_ms = 1000` (1 second)  
**Recommendation:** Provide a `low_latency` profile or reduce default to 200ms  
**Impact:** Reduces median end-to-end latency from ~515ms to ~115ms  
**Risk:** Higher CPU usage on scheduler process; mitigate with idle/active
detection (skip tick if no pending changes)  
**Trade-off:** Consider adaptive wake interval — 100ms when changes pending,
5000ms when idle

#### R4: SemiJoin Delta-Key Pre-Filtering (v0.11)

**Current:** SemiJoin Part 2 rescans full left table  
**Recommendation:** Implement O-1 from TPC-H benchmarking plan  
**Impact:** 15–26× improvement for Q18, Q20, Q21 type queries  
**Risk:** Low — restricted to SemiJoin operator diff, no architectural changes

#### ~~R5: Cost-Based Refresh Strategy Selection~~ — Done in v0.10.0

> **Status:** ✅ Implemented as item B-4 in v0.10.0. The `estimate_cost_based_threshold` +
> `compute_adaptive_threshold` functions apply a 60/40 blend of ratio-based and
> history-driven cost model from `pgt_refresh_history`; cold-start fallback to the
> fixed GUC threshold.

~~**Recommendation:** Integrate historical timing data more aggressively~~

### 10.2 Medium Priority — Scalability

#### R6: Partitioned Stream Tables (v0.11–v0.12)

**Current:** Stream tables are single unpartitioned tables  
**Recommendation:** Support RANGE/LIST/HASH partitioning for large stream
tables (10M+ rows)  
**Impact:** Partition-scoped MERGE reduces lock contention and enables
partition pruning at query time  
**Risk:** Medium — partition management adds complexity; need to handle
partition creation/detach in refresh cycle

#### R7: UNLOGGED-to-Logged Buffer Promotion (v0.12)

**Current:** Buffer partitioning available but off by default  
**Recommendation:** Default `buffer_partitioning = 'auto'` for sources with
high write throughput (detected by change buffer growth rate)  
**Impact:** Faster cleanup (DETACH+DROP vs TRUNCATE) and potential for
parallel scans of old partitions  
**Risk:** Low — partition lifecycle management already implemented

#### R8: Shared Change Buffers (v0.13)

**Current:** Each source table has its own change buffer  
**Recommendation:** Allow multiple STs watching the same source to share a
single change buffer (with per-ST LSN frontier tracking)  
**Impact:** Reduces write amplification for fan-out patterns (one source →
many STs)  
**Risk:** Medium — requires careful frontier management and concurrent
consumer coordination

#### R9: Increase Invalidation Ring Capacity (v0.11)

**Current:** 32-slot ring buffer  
**Recommendation:** Increase to 128 or 256 slots  
**Impact:** Reduces frequency of full DAG rebuilds under heavy DDL  
**Risk:** Minimal — slightly more shared memory; O(ring_size) dedup check is
still fast

### 10.3 Medium Priority — Safety & Ergonomics

#### R10: Enable Auto-Backoff by Default (v0.11)

**Current:** `auto_backoff = false`  
**Recommendation:** Change default to `true`  
**Impact:** Prevents scheduler overload when refresh consistently exceeds
schedule interval; reduces log noise and wasted CPU  
**Risk:** Low — well-tested since v0.4.0; users can still override  
**Rationale:** Auto-backoff is the safe behavior. The current default silently
wastes resources when refresh can't keep up.

#### R11: Enable Tiered Scheduling by Default (v0.12)

**Current:** `tiered_scheduling = false`  
**Recommendation:** Change default to `true` with sensible tier detection
(e.g., based on last-queried timestamp from pg_stat_user_tables)  
**Impact:** Automatically reduces refresh frequency for cold STs; improves
overall throughput for hot STs  
**Risk:** Low — users can override per-ST; adds modest complexity to
scheduler logic

#### R12: Block Source DDL by Default (v0.12)

**Current:** `block_source_ddl = false`  
**Recommendation:** Change default to `true` with clear error messages  
**Impact:** Prevents silent schema drift that breaks STs; forces explicit
ALTER STREAM TABLE after schema changes  
**Risk:** Medium — may surprise users who expect transparent DDL propagation.
Good error messages are essential.

#### R13: Wider Changed-Column Bitmask (v0.12)

**Current:** BIGINT bitmask limited to 63 columns  
**Recommendation:** Use array of BIGINTs or bytea for tables with >63 columns  
**Impact:** Column-level tracking for very wide tables  
**Risk:** Low — implementation is straightforward; most tables are <63 columns

### 10.4 Low Priority — Advanced Optimizations

#### R14: Async CDC via Custom Output Plugin (v0.14+)

**Current:** Text-based pgoutput parsing  
**Recommendation:** Custom output plugin with binary-efficient format  
**Impact:** Would eliminate text parsing overhead for WAL-based CDC  
**Risk:** High — requires C-level output plugin development and maintenance;
pgoutput is the standard

#### ~~R15: Multi-Table Delta Batching~~ — Done in v0.10.0

> **Status:** ✅ Implemented as items B3-2 + B3-3 in v0.10.0. Weight aggregation
> (`GROUP BY __pgt_row_id, SUM(weight) HAVING SUM(weight) != 0`) correctly replaces
> the incorrect DISTINCT ON approach. Six diamond-flow property-based tests verify
> correctness.

~~**Recommendation:** Merge delta computation for co-scheduled STs with shared sources~~

#### R16: Adaptive Scheduler Wake Interval (v0.12)

**Current:** Fixed 1000ms wake interval  
**Recommendation:** Event-driven wake using `pg_notify` from CDC triggers  
**Impact:** Near-zero latency when changes appear; near-zero CPU when idle  
**Risk:** Medium — adds trigger→scheduler signaling path; needs NOTIFY
coalescing to avoid thundering herd

#### ~~R17: MERGE Bypass for Append-Only Patterns~~ — Done in v0.5.0

> **Status:** ✅ Implemented as item A-3a in v0.5.0. The `APPEND ONLY` declaration on
> `CREATE STREAM TABLE` enables a direct INSERT path (no MERGE). A heuristic
> automatically reverts to full MERGE on first observed DELETE or UPDATE, emitting
> a `WARNING` + `pgtrickle_alert` NOTIFY.

~~**Recommendation:** Use direct INSERT for pure append patterns~~

#### ~~R18: Index-Aware MERGE Planning~~ — Done in v0.10.0

> **Status:** ✅ Implemented as item A-4 in v0.10.0. `SET LOCAL enable_seqscan = off`
> is injected when delta row count is below `merge_seqscan_threshold` (0.001 × ST
> size). Covering index auto-created on `__pgt_row_id` with INCLUDE clause for
> ≤8-column schemas. Reverts at transaction end via `SET LOCAL` (not `SET`).

~~**Recommendation:** Hint index usage based on change buffer PK distribution~~

---

## 11. Summary & Prioritized Roadmap

### Immediate Wins (v0.11)

| ID | Recommendation | Effort | Impact |
|----|---------------|--------|--------|
| R1 | Parallel refresh on by default | Low | 2.5–5× throughput |
| R4 | SemiJoin delta-key pre-filter | Medium | 15–26× for affected queries |
| R9 | Larger invalidation ring | Trivial | Prevents full DAG rebuilds |
| R10 | Auto-backoff on by default | Trivial | Prevents scheduler overload |

### Near-Term (v0.12)

| ID | Recommendation | Effort | Impact |
|----|---------------|--------|--------|
| R2 | UNLOGGED change buffers | Medium | ~30% CDC write reduction |
| R3/R16 | Adaptive scheduler wake interval | Medium | 4× latency reduction |
| R6 | Partitioned stream tables | High | Unlocks 10M+ row STs |
| R7 | Auto buffer partitioning for high-throughput sources | Low | Faster cleanup |
| R11 | Tiered scheduling on by default | Low | Resource efficiency |
| R12 | Block source DDL by default | Low | Safety |
| R13 | Wider column bitmask (>63 columns) | Low | Wide table support |

### Medium-Term (v0.13–v0.14)

| ID | Recommendation | Effort | Impact |
|----|---------------|--------|--------|
| R8 | Shared change buffers | Medium | Fan-out efficiency |

### Long-Term (v1.0+)

| ID | Recommendation | Effort | Impact |
|----|---------------|--------|--------|
| R14 | Custom WAL output plugin | Very High | Optimal CDC throughput |
| — | Distributed stream tables | Very High | Multi-node scalability |
| — | External orchestrator | High | 100+ ST deployments |

---

## Appendix A: Version History (Performance Milestones)

| Version | Date | Performance Milestone |
|---------|------|----------------------|
| v0.1.0 | 2026-02-26 | Baseline DVM engine, trigger CDC |
| v0.1.3 | 2026-03-02 | JSONB fix, delta SQL caching (2.7× improvement) |
| v0.2.0 | 2026-03-04 | TopK, diamond consistency, IMMEDIATE mode |
| v0.3.0 | 2026-03-11 | Correctness fixes, HAVING, FULL OUTER JOIN |
| v0.4.0 | 2026-03-12 | Parallel refresh, statement-level triggers, prepared statements |
| v0.5.0 | 2026-03-13 | Append-only fast path, source gating |
| v0.6.0 | 2026-03-14 | Partitioned tables, idempotent DDL |
| v0.7.0 | 2026-03-15 | Watermarks, circular deps, xxHash for wide tables |
| v0.8.0 | 2026-03-17 | Backup/restore, reliability hardening |
| v0.9.0 | 2026-03-20 | Algebraic aggregates (O(1) AVG/STDDEV/VAR), predicate pushdown |
| v0.10.0 | 2026-03-23 | Covering indexes (20–50%), compaction (50–90%), PgBouncer support |

## Appendix B: Benchmark Matrix

**5 scenarios × 3 change rates × 2 table sizes = 30 combinations**

Scenarios: scan, filter, aggregate, join, join_agg  
Change rates: 1%, 10%, 50%  
Table sizes: 10K, 100K rows  

Additional benchmarks:
- TPC-H (22 queries at SF-0.01 and SF-0.1)
- Trigger overhead (narrow/medium/wide × INSERT/UPDATE/DELETE)
- Criterion.rs micro-benchmarks (DVM operators, utility functions)
- Continuous regression detection via Bencher.dev

## Appendix C: GUC Quick Reference (Performance-Relevant)

| GUC | Default | Tuning Guidance |
|-----|---------|-----------------|
| `scheduler_interval_ms` | 1000 | Lower for fresher data; higher for less CPU |
| `differential_max_change_ratio` | 0.15 | Lower = more FULL refreshes; higher = more DIFF |
| `merge_work_mem_mb` | 64 | Increase for large deltas (sorting) |
| `merge_seqscan_threshold` | 0.001 | Disables seqscan when delta is tiny fraction of ST |
| `parallel_refresh_mode` | off | Set to 'on' for independent ST graphs |
| `max_dynamic_refresh_workers` | 4 | Match to available CPU cores |
| `auto_backoff` | false | Enable if refresh often exceeds schedule |
| `tiered_scheduling` | false | Enable for large ST counts with mixed access patterns |
| `compact_threshold` | 100000 | Lower for faster buffers; 0 to disable |
| `buffer_partitioning` | off | Enable for high-throughput sources |
| `cleanup_use_truncate` | true | Keep true unless using change history |

---

## 12. Deep Gap Analysis — DVM Correctness & SQL Coverage

*Added 2026-03-24. Findings from deep source-level audit of `src/dvm/`,
`src/refresh.rs`, and `src/api.rs`.*

### 12.1 Known Correctness Boundaries

#### G12-1: EC-01 Phantom-Row-After-DELETE for ≥3-Scan Right Subtrees

**Severity:** Medium (silent incorrect result)
**File:** `src/dvm/operators/join_common.rs:1052–1088`

The EC-01 fix (R₀ via EXCEPT ALL) is gated at `join_scan_count(child) <= 2`.
Join subtrees with ≥3 scan nodes on the right side — TPC-H Q7, Q8, Q9 all
qualify — retain the original phantom-row-after-DELETE bug: when both the left
and right rows of a join are deleted in the same batch, the left-side DELETE
can be silently dropped because R₁ (post-change right) no longer contains
the partner row.

The threshold exists because EXCEPT ALL materializes the full right-side
snapshot for wide join trees, causing PostgreSQL to spill multi-GB temp files.
The `NOT MATERIALIZED` CTE hint partially mitigates this but doesn't fully
solve the cascading materialization problem for deep join trees.

**Impact:** Incorrect results for multi-way joins under simultaneous
cross-table DELETE. Affects any join chain where the right subtree has ≥3
leaf scans.

**Recommendation:** Design a per-subtree CTE-based snapshot strategy to
replace EXCEPT ALL for deep join trees. Track as EC01B (v0.12.0).

#### G12-2: TopK Refresh Assumption Violation

**File:** `src/refresh.rs:1144–1200`

TopK refresh assumes `LIMIT` without `OFFSET` or correlated WHERE conditions.
If violated, the TopK micro-refresh may silently produce wrong results.
No explicit runtime check validates this assumption.

**Impact:** Silent correctness bug if TopK path is reached with unsupported
LIMIT patterns.

**Recommendation:** Add an assertion guard that validates TopK eligibility at
refresh time, not just at creation time. Fall back to FULL refresh if
assumptions are violated. Low effort (~2h).

#### G12-3: MERGE Duplicate-Row Error on Concurrent Changes

**File:** `src/refresh.rs:738–750`

When multiple delta rows target the same ST row (e.g., rapid
INSERT→UPDATE→DELETE within one scheduler tick), PostgreSQL rejects with
"MERGE command cannot affect row a second time." The current fix materializes
a deduplicated CTE before MERGE.

**Impact:** Forces materialization of the entire delta, defeating streaming
optimization. For large deltas (100K+ rows), this adds measurable latency.

**Recommendation:** Investigate a two-pass MERGE approach: first-pass applies
non-conflicting rows directly, second-pass handles the deduplicated residual.
Alternatively, explore PostgreSQL 18's `MERGE ... ON CONFLICT` path if available.

### 12.2 SQL Support Gaps

| SQL Feature | Status | Severity | Notes |
|-------------|--------|----------|-------|
| **VALUES in FROM** | Rejected | Low | `Unsupported FROM item (VALUES clause...)` — rare in analytical queries |
| **TABLESAMPLE** | Rejected | Low | Non-deterministic by nature; documented rejection |
| **Nested aggregates** | Rejected | Medium | `AVG(SUM(...))` — requires subquery rewrite; users can work around |
| **Non-equijoin conditions** | Partial | Medium | `>`, `<`, `LIKE` conditions in JOIN ON are not differentiated; may silently produce wrong results or fall back to FULL |
| **NATURAL JOIN** | Warned | Low | F38 warning about column drift on ALTER; DIFFERENTIAL still works |
| **VALUES() constructor in expressions** | Rejected | Low | `VALUES (1),(2),(3)` as standalone FROM rejected |
| **JSON_TABLE advanced** | Partial | Low | Basic structure parsed; complex column definitions may be silently dropped |
| **JSON_ARRAYAGG / JSON constructor ops** | Partial | Low | PG 18 SQL/JSON operators may not be fully recognized |
| **Multi-column IN subquery** | Unclear | Medium | `EXPR IN (subquery)` where subquery returns multiple columns — status unclear; may produce wrong results |

### 12.3 Aggregate Performance Classification

25 aggregate functions use **group-rescan** (O(group_size)) rather than
algebraic O(1) maintenance:

| Category | Functions | Strategy |
|----------|-----------|----------|
| **Algebraic (O(1))** | SUM, COUNT, AVG, STDDEV_*, VAR_*, COVAR_*, REGR_*, CORR | Auxiliary columns |
| **Min/Max (rescan)** | MIN, MAX | Group-rescan (removing min/max requires full scan) |
| **Group-rescan** | STRING_AGG, ARRAY_AGG, JSON[B]_AGG, JSON[B]_OBJECT_AGG, JSON_OBJECTAGG_STD, JSON_ARRAYAGG_STD, BIT_AND, BIT_OR, BIT_XOR, BOOL_AND, BOOL_OR, XMLAGG, ANY_VALUE, MODE, PERCENTILE_CONT, PERCENTILE_DISC, HYP_RANK, HYP_DENSE_RANK, HYP_PERCENT_RANK, HYP_CUME_DIST, ComplexExpression, UserDefined | Full group rescan per affected group |

The group-rescan strategy is correct but expensive for queries with many
groups and frequent changes. Users are not warned at creation time that their
aggregate choice has O(group_size) refresh cost.

**Recommendation:**
- Emit a WARNING at `create_stream_table` time when group-rescan aggregates
  are used in DIFFERENTIAL mode, noting the performance implication.
- Expose aggregate strategy classification in `explain_st()`.
- Long-term: implement Welford-style algebraic maintenance for BIT_AND/BIT_OR
  and BOOL_AND/BOOL_OR (these support algebraic inversion).

### 12.4 Silent AUTO Mode Downgrades

When `refresh_mode = 'AUTO'` (default), the system silently downgrades from
DIFFERENTIAL to FULL in 6+ scenarios:

1. Window functions in expressions (EC-03 lift)
2. Unsupported DVM constructs detected
3. DVM parser failure
4. Materialized views / foreign tables as sources
5. Nested window rewrite fallback
6. Non-deterministic expression detected (for some categories)

These downgrades emit `pgrx::info!()` messages which are at LOG level and
typically invisible to interactive users.

**Impact:** Users experience unexplained performance regressions. The
"effective" refresh mode is not stored in the catalog — only the requested
mode is visible.

**Recommendation:**
- Store `effective_refresh_mode` in `pgt_stream_tables` catalog.
- Add `explain_refresh_mode(name)` SQL function that returns a human-readable
  explanation of why the current mode was selected.
- Upgrade mode-downgrade messages from `info!()` to `warning!()`.

---

## 13. Deep Gap Analysis — Safety & Code Quality

*Added 2026-03-24. Findings from audit of `src/` for panics, unwraps,
code quality, and architectural concerns.*

### 13.1 Production Panic Sites

| File | Line | Code | Risk |
|------|------|------|------|
| `scheduler.rs` | 2358 | `unwrap_or_else(\|_\| panic!("check_skip_needed: SPI select failed"))` | **Real** — SPI failure during scheduler tick crashes the background worker |
| `refresh.rs` | 3051, 3068 | `panic!("expected InvalidArgument, got {other:?}")` | Test-only (`#[cfg(test)]`) — no production risk |

**Assessment:** Only 1 real production panic path confirmed. However, the
broader audit found **65 `unwrap()`/`expect()` calls** across core modules
(`scheduler.rs`, `hooks.rs`, `cdc.rs`, `catalog.rs`, `refresh.rs`, `api.rs`).
Most are in test code, but several are in production paths protected only by
PostgreSQL invariant assumptions (e.g., "FROM list always has ≥1 item").

### 13.2 Unwrap Calls in Parser (Production Risk)

| Location | Code | Justification |
|----------|------|---------------|
| `parser.rs:7123` | `func_list.head().unwrap()` | PG guarantees non-empty FuncCall list |
| `parser.rs:9048` | `from_list.head().unwrap()` | FROM always has ≥1 item |
| `parser.rs:9389` | `target_list.head().unwrap()` | SELECT always has ≥1 target |
| `parser.rs:14324` | `.expect("at least one predicate")` | BoolExpr flattening assumes ≥1 arg |

These are justified by PostgreSQL parser guarantees but lack `// SAFETY:`
comments. If PostgreSQL internals change (e.g., PG 19 parse tree changes),
these could become panics without any compile-time or test-time warning.

**Recommendation:**
- Add `// INVARIANT: PostgreSQL parser guarantees <reason>` comments to each.
- Convert to `.ok_or(PgTrickleError::InternalError(...))` for defense-in-depth,
  especially before PG 19 backward-compatibility work begins.

### 13.3 println! in Production Code

| File | Line | Issue |
|------|------|-------|
| `refresh.rs` | 2269 | `println!("MERGE SQL TEMPLATE:\n{}", merge_template)` — unguarded debug output |

This prints the full MERGE SQL template to stdout on every refresh cycle.
In a production PostgreSQL deployment, this goes to the server log at an
uncontrolled level.

**Recommendation:** Replace with `pgrx::log!()` guarded by a
`pg_trickle.log_merge_sql` GUC (default `false`). Effort: ~30 min.

### 13.4 Parser File Complexity

`src/dvm/parser.rs` is 19,674 lines — by far the largest file in the project
(25% of total source). Key concerns:

- Three separate `from_item_to_sql()` variants (lines ~7970, ~8914, ~12449)
  with overlapping logic.
- Deep parse tree recursion (5–7 levels common) making stack overflow
  theoretically possible for pathological queries.
- 664 `unsafe` blocks (95% of all unsafe in the project), all wrapping
  `pg_sys` FFI calls with proper SAFETY comments.
- Limited macro extraction for visitor patterns — repetitive match arms for
  node type dispatch.

**Recommendation:** Not urgent for correctness, but refactoring into
sub-modules (by SQL construct: joins, aggregates, subqueries, CTEs, window
functions) would improve maintainability and make the PG 19 compatibility
work (BC2) significantly easier.

### 13.5 Error Message Quality

Error messages are technically accurate but not actionable:

| Current Message | Problem |
|----------------|---------|
| `"unsupported operator for DIFFERENTIAL mode: {0}"` | No suggested fix or alternative |
| `"cycle detected in dependency graph: A → B → C"` | No guidance on how to break the cycle |
| `"upstream table schema changed: OID {0}"` | OID is meaningless to users |
| `"query parse error: {0}"` | No indication of which part of the query failed |

**Recommendation:** Add "HINT" text to error messages at the API boundary.
PostgreSQL supports structured error messages with `DETAIL` and `HINT` fields.
Use `ereport!(ERROR, ... HINT: "...")` instead of raw `error!()`. Example:
```
ERROR: unsupported operator for DIFFERENTIAL mode: TABLESAMPLE
HINT: TABLESAMPLE produces non-deterministic results. Use refresh_mode =>
'FULL' or remove the TABLESAMPLE clause.
```

### 13.6 Memory Ordering

Shared memory atomics in `src/shmem.rs` use correct ordering:
- `Relaxed` for monotonic counters (`CACHE_GENERATION`) — correct
- `AcqRel`/`Acquire` for CAS operations (worker tokens) — correct

No issues found.

---

## 14. Deep Gap Analysis — Performance Opportunities

*Added 2026-03-24.*

### 14.1 CDC Write-Side Overhead (Unmeasured)

The write-side cost of CDC triggers has never been benchmarked.
`plans/performance/PLAN_TRIGGERS_OVERHEAD.md` defines a 5-scenario benchmark
plan but has no status and is not referenced in the roadmap.

**Impact:** Without data, we cannot make informed decisions about:
- Whether `change_buffer_unlogged` GUC is worth implementing
- Whether statement-level triggers are sufficient for high-throughput OLTP
- Whether the covering index strategy is optimal

**Recommendation:** Implement the trigger overhead benchmark as BENCH-W1/W2
(tracked in v0.12.0 roadmap).

### 14.2 IMMEDIATE Mode Performance

IMMEDIATE mode (transactional IVM) has several deferred Phase 4 optimizations:

| Item | Description | Impact | Status |
|------|-------------|--------|--------|
| ENR-based transition tables | Replace PL/pgSQL temp table copy with Ephemeral Named Relations | Eliminates CREATE/DROP TEMP TABLE overhead per trigger invocation | Not started (requires `unsafe` pg_sys ENR APIs) |
| C-level trigger functions | Replace PL/pgSQL wrapper with C/Rust function | Eliminates PL/pgSQL interpreter overhead | Not started (very high complexity) |
| Aggregate fast-path | Single `UPDATE` for invertible aggregates instead of full delta SQL | O(1) per-DML for simple aggregates | Not started |
| Prepared statement reuse | Keep SPI prepared statements across invocations in same transaction | Eliminates re-parse per trigger call | Not started |

These are tracked as A2 in the post-1.0 roadmap. For IMMEDIATE-heavy
workloads, the PL/pgSQL wrapper is the dominant cost.

### 14.3 Group-Rescan Aggregate Cost

25 aggregate functions use group-rescan (see §12.3). For queries like:
```sql
SELECT department, STRING_AGG(name, ', ' ORDER BY name)
FROM employees GROUP BY department
```
every change to `employees` triggers a full re-aggregation of the affected
department group. With 1000 employees per department, this is 1000× more
expensive than an algebraic SUM.

**Quick wins:**
- `BIT_AND`/`BIT_OR`/`BIT_XOR` — these are algebraically invertible.
  `BIT_AND` requires auxiliary column tracking the bitwise AND of all values;
  removal of a value requires full rescan only if the removed value was the
  sole contributor to a 0 bit. Due to the complexity of tracking per-bit
  contributors, these are correctly classified as group-rescan for now.
- `BOOL_AND`/`BOOL_OR` — similar to BIT_AND/BIT_OR but on single bits.
  Algebraic maintenance would require tracking the count of TRUE values per
  group. Feasible but low priority.

### 14.4 MERGE Template Deduplication Overhead

When multiple delta rows target the same ST `__pgt_row_id`, the entire delta
must be materialized and deduplicated before MERGE (§12.3). This prevents
PostgreSQL from streaming the delta directly into the MERGE executor.

For workloads with rapid UPDATE→UPDATE sequences (e.g., counters, status
fields), this materialization can dominate refresh time.

**Recommendation:** Profile the frequency of delta deduplication across
real-world workloads. If ≥10% of refreshes need dedup, investigate a
pre-MERGE compaction pass in the change buffer itself (cancel out
INSERT→DELETE pairs before delta SQL generation — partially implemented
as B3-1 compaction but not integrated with MERGE dedup).

### 14.5 Thread-Local Cache Cold-Start

Delta SQL templates and MERGE plans are cached per backend session via
`thread_local!`. Cross-session invalidation uses a shared atomic counter
(`CACHE_GENERATION`), but every new connection pays a cold-cache penalty of
~45ms on first refresh.

For connection-pooler deployments (PgBouncer) where connections are reused
across many sessions, this is amortized. But for serverless or
short-connection workloads, the cold-start cost is significant.

**Recommendation:** Investigate shared-memory template caching using
PostgreSQL's shared buffer mechanism. This would eliminate per-connection
cold-start entirely but requires careful invalidation management.

---

## 15. Deep Gap Analysis — Ergonomics & API Design

*Added 2026-03-24.*

### 15.1 Function Parameter Overload

`create_stream_table()` has 10 parameters with 7 optional:

```sql
SELECT pgtrickle.create_stream_table(
  name, query,
  schedule?, refresh_mode?, initialize?,
  diamond_consistency?, diamond_schedule_policy?,
  cdc_mode?, append_only?, pooler_compatibility_mode?
);
```

Related parameters (e.g., `diamond_consistency` and
`diamond_schedule_policy`) are separated without logical grouping. Discovery
of advanced options like `pooler_compatibility_mode` requires reading the
full SQL reference.

**Recommendation:**
- Provide configuration profiles: `create_stream_table(name, query,
  profile => 'low_latency')` that set multiple GUCs at once.
- Document the "80% use case" prominently: most users only need
  `(name, query)` or `(name, query, schedule)`.

### 15.2 Invisible Query Rewrite Pipeline

The 8-pass query rewrite pipeline transforms the user's query before DVM
parsing:

1. View inlining (fixpoint)
2. Nested window expression lifting
3. DISTINCT ON → ROW_NUMBER rewrite
4. GROUPING SETS/CUBE/ROLLUP expansion
5. Scalar subquery decorrelation (WHERE)
6. Correlated scalar decorrelation (SELECT)
7. SubLink-in-OR rewriting
8. ROWS FROM transformation

Users never see the rewritten query, making debugging impossible when the
DVM engine produces unexpected results or falls back to FULL mode.

**Recommendation:** Add `explain_query_rewrite(query TEXT)` SQL function that
returns the rewritten query and a log of which passes were applied. This is
pure diagnostics — no production impact.

### 15.3 Missing Convenience Functions

| Function | Use Case | Priority |
|----------|----------|----------|
| `explain_refresh_mode(name)` | Show why DIFFERENTIAL or FULL was chosen | High |
| `explain_query_rewrite(query)` | Show query after rewrite pipeline | High |
| `diagnose_errors(name)` | Show last N errors + root cause + suggested fixes | High |
| `list_auxiliary_columns(name)` | Show hidden `__pgt_*` columns and their purpose | Medium |
| `validate_query(query)` | Dry-run validation without creating the ST | Medium |
| `explain_refresh_cost(name)` | Estimate refresh cost based on change buffer size | Low |
| `bulk_create(definitions JSONB)` | Create multiple STs in one call with DAG ordering | Low |
| `export_definition(name)` | Export ST definition as reproducible SQL | Low |

### 15.4 Error Recovery Experience

When a stream table enters ERROR status (consecutive_errors ≥ 3):
- `pgt_status()` shows `status = 'ERROR'` but no root cause
- Must manually `ALTER STREAM TABLE ... status => 'ACTIVE'` to retry
- No indication of whether the root cause is fixed

**Recommendation:** The `diagnose_errors(name)` function should return:
- Last 5 error messages from `pgt_refresh_history`
- Error classification (user/schema/system)
- Whether the error is retryable
- Suggested fix for each error
- Whether the root cause has been resolved (e.g., dropped table restored)

### 15.5 Configuration Interaction Effects

23 GUCs with undocumented interaction effects:

| Interaction | Effect | Documentation |
|-------------|--------|---------------|
| `scheduler_interval_ms` + `min_schedule_seconds` + `default_schedule_seconds` | Three separate timers that determine actual refresh frequency; unclear which takes precedence | Not documented |
| `differential_max_change_ratio` + `merge_seqscan_threshold` + `auto_backoff` + `merge_planner_hints` | All affect DIFFERENTIAL performance; users don't know which to tune | Not documented as a group |
| `cdc_mode='wal'` + `refresh_mode='IMMEDIATE'` | Invalid combination; only caught at runtime | Not documented |
| `append_only=true` + DELETE on source | Silently reverts to `false` with no warning | Not documented |
| `diamond_schedule_policy` + `diamond_consistency='off'` | Policy is ignored when consistency is off | Not obvious |

**Recommendation:** Create a GUC interaction matrix in CONFIGURATION.md.
Provide "tuning profiles" for common scenarios: low-latency, high-throughput,
resource-constrained.

### 15.6 Auxiliary Column Visibility

Stream tables contain hidden `__pgt_*` columns that surprise users:

| Column | Purpose | When Present |
|--------|---------|-------------|
| `__pgt_row_id` | Row identity for MERGE | Always |
| `__pgt_count` | Multiplicity for DISTINCT | When DISTINCT present |
| `__pgt_count_l`, `__pgt_count_r` | Join multiplicity | When FULL JOIN present |
| `__pgt_union_dedup_count` | UNION deduplication | When UNION (not UNION ALL) present |
| `__pgt_aux_count_*` | AVG auxiliary counter | When AVG present |
| `__pgt_aux_sum_*` | STDDEV/VAR auxiliary | When STDDEV/VAR present |
| `__pgt_aux_sum2_*` | STDDEV/VAR sum-of-squares | When STDDEV/VAR present |
| `__pgt_aux_nonnull_*` | NULL transition counter | When SUM over FULL JOIN |
| `__pgt_aux_sum[xy]_*` | COVAR/REGR auxiliary | When COVAR/REGR present |

Users writing `SELECT *` get unexpected columns. The FAQ mentions this briefly
but the SQL reference doesn't document them.

**Recommendation:**
- Document in SQL_REFERENCE.md under its own section.
- Add `list_auxiliary_columns(name)` function.
- Consider a view layer (`pgtrickle.user_view_<name>`) that excludes
  `__pgt_*` columns, or add pg_attribute marking to hide them from
  `SELECT *` (using `attisdropped`-adjacent mechanism).

---

## 16. Deep Gap Analysis — Documentation & Onboarding

*Added 2026-03-24.*

### 16.1 Getting Started Complexity

The GETTING_STARTED.md guide jumps directly to a 3-layer recursive department
tree with CTEs, multi-table joins, and cascading refreshes. Most users' first
stream table will be a simple aggregation.

**Recommendation:** Restructure as progressive complexity:
1. **Hello World** — single-table aggregation (`GROUP BY + SUM`)
2. **Two-table join** — orders + customers
3. **Scheduling** — `calculated` vs cron vs duration
4. **Monitoring** — `pgt_status()`, `explain_st()`, `dependency_tree()`
5. **Multi-layer** — chained STs, diamond patterns
6. **Advanced** — recursive CTEs, IMMEDIATE mode

### 16.2 Missing Operator Support Matrix

No single document answers "Does my query support DIFFERENTIAL mode?" Users
must cross-reference SQL_REFERENCE.md, DVM_OPERATORS.md, and FAQ.md.

**Recommendation:** Add to DVM_OPERATORS.md a support matrix:

| SQL Feature | DIFFERENTIAL | FULL | IMMEDIATE | Notes |
|-------------|:---:|:---:|:---:|-------|
| Inner JOIN | ✅ | ✅ | ✅ | |
| LEFT JOIN | ✅ | ✅ | ✅ | |
| FULL OUTER JOIN | ✅ | ✅ | ✅ | SUM/AVG may trigger group-rescan |
| Semi/Anti JOIN | ✅ | ✅ | ✅ | |
| LATERAL | ✅ | ✅ | ✅ | |
| Window functions | ⚠️ | ✅ | ✅ | In expressions → FULL fallback |
| Recursive CTE | ✅ | ✅ | ✅ | Non-monotone rejected |
| VALUES in FROM | ❌ | ✅ | ❌ | Rejected |
| TABLESAMPLE | ❌ | ❌ | ❌ | Non-deterministic |
| Nested aggregates | ❌ | ✅ | ❌ | Requires subquery rewrite |

### 16.3 Missing Best-Practice Patterns

No documentation covers common data architecture patterns:

| Pattern | Description | Status |
|---------|-------------|--------|
| Bronze → Silver → Gold | Multi-layer ST pipeline for data warehouse | Not documented |
| Event sourcing | Base table as event log → ST as current state | Not documented |
| Slowly Changing Dims | Type-1/Type-2 SCD patterns with STs | Not documented |
| Fan-out (one → many) | One source feeding multiple STs | Briefly mentioned |
| Diamond convergence | A → B,C → D pattern | Documented in ARCHITECTURE |
| Hot/warm/cold tiering | Different refresh rates by access frequency | GUC exists, pattern not documented |

### 16.4 Monitoring Function Discovery

GETTING_STARTED.md ends with `pgt_status()` but doesn't introduce the 15+
monitoring functions available:

| Function | Purpose | Discoverability |
|----------|---------|-----------------|
| `pgt_status()` | Current status | ✅ In GETTING_STARTED |
| `explain_st(name)` | Deep inspect | ❌ Only in SQL_REFERENCE |
| `dependency_tree(name)` | DAG visualization | ❌ Only in SQL_REFERENCE |
| `check_cdc_health()` | CDC status | ❌ Only in SQL_REFERENCE |
| `st_refresh_stats(name)` | Refresh timing | ❌ Only in SQL_REFERENCE |
| `get_refresh_history(name)` | History | ❌ Only in SQL_REFERENCE |
| `health_check()` | Overall health | ❌ Only in SQL_REFERENCE |
| `change_buffer_sizes()` | Buffer stats | ❌ Only in SQL_REFERENCE |
| `list_sources(name)` | Source tables | ❌ Only in SQL_REFERENCE |
| `trigger_inventory()` | CDC triggers | ❌ Only in SQL_REFERENCE |
| `worker_pool_status()` | Parallel workers | ❌ Only in SQL_REFERENCE |
| `refresh_timeline(name)` | Refresh history timeline | ❌ Only in SQL_REFERENCE |
| `slot_health()` | WAL slot health | ❌ Only in SQL_REFERENCE |
| `watermark_status()` | Watermark state | ❌ Only in SQL_REFERENCE |
| `st_auto_threshold(name)` | Adaptive threshold | ❌ Only in SQL_REFERENCE |

**Recommendation:** Add a "Monitoring Quick Reference" section to
GETTING_STARTED.md with the 5 most useful functions and links to the full
reference.

---

## 17. Deep Gap Analysis — Testing & Verification

*Added 2026-03-24.*

### 17.1 Test Inventory

| Tier | Count | Focus |
|------|-------|-------|
| Unit tests | 1,194 | SQL parsing, OpTree, diff operators, helpers |
| Integration tests | 101 | Bare PG catalog schema behavior |
| E2E tests | 340+ | Full extension (CDC, refresh, background workers) |
| **Total** | **1,635+** | |

**Source:** 78,539 lines Rust source / 59,870 lines Rust tests
(test:source ratio = 0.76:1).

### 17.2 Coverage Ceiling

Structural code coverage is estimated at ~63%. The ceiling is caused by:
- Error handling paths rarely triggered in tests
- Platform-specific code (`#[cfg(target_os)]`)
- Emergency fallback paths
- Logging-only code branches

### 17.3 Missing Test Coverage Areas

| Area | Gap | Priority | Effort |
|------|-----|----------|--------|
| **External SQL corpora** | No sqllogictest, JOB, or Nexmark integration | High | 1–2 weeks |
| **Differential fuzzing** | No SQLancer or property-based SQL generation | High | 1–2 weeks |
| **Property tests (items 5+6)** | Topology stress + DAG helper properties not started | Medium | 6–10 days |
| **Write-side benchmarks** | CDC trigger overhead never measured | Medium | 3–5 days |
| **IMMEDIATE mode stress** | No high-concurrency IMMEDIATE mode test (100+ concurrent DML) | Medium | 2–3 days |
| **PG version matrix** | Only PG 18 tested; PG 16/17 untested | Medium | 2–3 weeks (with CI matrix) |
| **Multi-database** | No test for scheduler across multiple databases | Low | 1–2 days |
| **Long-running stability** | No soak test (24h+ continuous refresh) | Low | 1–2 days |

### 17.4 Ignored Test Suites

| File | Ignored Count | Reason |
|------|--------------|--------|
| `e2e_tpch_tests.rs` | 11 | TPC-H benchmarks (not regression); run separately |
| `e2e_bench_tests.rs` | 16 | Performance microbenchmarks; run separately |
| `e2e_upgrade_tests.rs` | 8 | Requires separate Docker upgrade image build |

All `#[ignore]` annotations are intentional. No accidentally-ignored tests
found.

### 17.5 Testing Infrastructure Strengths

- Testcontainers-based E2E with custom Docker images
- Property-based testing with proptest (4/7 planned items done)
- TPC-H validation at SF-0.01 and SF-0.1
- Criterion.rs micro-benchmarks with Bencher.dev regression detection
- Light E2E tier using `cargo pgrx package` for fast PR validation
- 6-tier test pyramid (unit → integration → light-E2E → full-E2E → TPC-H → dbt)

### 17.6 Verification Gaps

| Gap | Risk | Recommendation |
|-----|------|----------------|
| No offline MERGE SQL validation | Generated SQL could be syntactically invalid for edge-case column names | Add a `EXPLAIN (COSTS OFF)` dry-run for generated MERGE templates during E2E tests |
| No stack-depth limit for parser recursion | Pathological queries could cause stack overflow in the 19.7K-line parser | Add a depth counter to parse tree visitors; reject queries exceeding limit |
| No regression test for EC-01 boundary | ≥3-scan right subtrees are documented as limited but no test asserts the boundary | Add a negative test that demonstrates the known limitation with a comment explaining the boundary |

---

## Appendix D: Codebase Metrics

| Metric | Value |
|--------|-------|
| Source code (Rust) | 78,539 lines |
| Test code (Rust) | 59,870 lines |
| Test:source ratio | 0.76:1 |
| Largest file | `parser.rs` (19,674 lines, 25% of source) |
| Total `unsafe` blocks | 695 (664 in `parser.rs` alone) |
| Total `pg_extern` functions | 43 (28 in `api.rs`, 15 in `monitor.rs`) |
| Total GUCs | 23 |
| DVM operators | 21 |
| Aggregate functions | 60+ |
| Edge cases catalogued | 36 (EC-01 through EC-36) |
| Plans in `plans/` | 50+ files |
| Plans complete | ~30 |
| Plans in progress | ~5 |
| Plans proposed/deferred | ~15 |
