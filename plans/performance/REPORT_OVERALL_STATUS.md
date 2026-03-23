# pg_trickle — Overall Project Status & Performance Report

**Date:** 2026-03-23  
**Version:** 0.10.0  
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
| Correctness | 36 edge cases catalogued; all P0 (data loss) resolved | ★★★★★ |
| Performance | 7–42× speedup at 1% change rate; MERGE is 70–97% of time | ★★★★☆ |
| Defaults | Conservative and safe; advanced features opt-in | ★★★★★ |
| Scalability | Parallel refresh implemented but off by default | ★★★☆☆ |
| Safety | 51% unsafe reduction; no unwrap/panic in SQL paths | ★★★★☆ |
| Ergonomics | `calculated` keyword, warnings, quick_health view | ★★★★☆ |
| Observability | NOTIFY alerts, refresh stats, health checks, dependency tree | ★★★★☆ |
| SQL Coverage | 21 operators, 60+ aggregate functions, recursive CTEs | ★★★★★ |
| Deployment | PgBouncer, CNPG, backup/restore, dbt integration | ★★★★☆ |

**Overall assessment:** The project is production-ready for typical analytical
workloads. The primary performance bottleneck is PostgreSQL's own MERGE
execution (70–97% of refresh time), meaning the Rust-side pipeline is highly
optimized. The main opportunities for improvement are in scaling to larger
deployments (100+ stream tables), reducing CDC write-side overhead, and
unlocking the parallel refresh feature by default.

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
