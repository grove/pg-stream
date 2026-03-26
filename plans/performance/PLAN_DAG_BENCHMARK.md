# DAG Topology Benchmarking Suite

> **Status:** Plan  
> **Date:** 2026-03-26  
> **Related:** [PLAN_DAG_PERFORMANCE.md](PLAN_DAG_PERFORMANCE.md) ·
> [STATUS_PERFORMANCE.md](STATUS_PERFORMANCE.md) ·
> [docs/BENCHMARK.md](../../docs/BENCHMARK.md)

---

## 1. Motivation

The existing E2E benchmark suite (`e2e_bench_tests.rs`) measures **single-ST
refresh performance** — per-operator speed at varying table sizes and change
rates. However, production deployments form DAGs with 10–500+ stream tables
arranged in chains, fan-outs, diamonds, and mixed topologies. The
[DAG Performance Analysis](PLAN_DAG_PERFORMANCE.md) provides theoretical
latency formulas for five topology patterns and three refresh modes, but we
have **no automated way to validate these projections** or detect regressions
in DAG propagation overhead.

### Gap

| Existing coverage | Missing coverage |
|-------------------|-----------------|
| Single-ST refresh (scan, filter, join, …) | End-to-end propagation through multi-level DAGs |
| Table size and change rate sensitivity | Topology shape sensitivity (depth, width, branching) |
| FULL vs INCREMENTAL comparison | CALCULATED vs PARALLEL mode comparison |
| Per-phase profile (decision, merge, cleanup) | Per-hop latency breakdown across DAG levels |
| Criterion micro-benchmarks (operators, utilities) | Scheduler overhead and poll-interval impact |

### Goals

1. Validate the latency formulas from PLAN_DAG_PERFORMANCE.md §5 against
   real measurements.
2. Provide regression detection for DAG propagation overhead.
3. Compare CALCULATED vs PARALLEL refresh mode across topologies.
4. Measure both **latency** (auto-refresh, source-INSERT → leaf-ST-updated)
   and **throughput** (manual refresh, changes-per-second through full DAG).

---

## 2. Design

### 2.1 File Location & Test Tier

- **File:** `tests/e2e_dag_bench_tests.rs`
- **Tier:** Full E2E — all tests `#[ignore]`, require the `pg_trickle_e2e`
  Docker image
- **Runner:** `just test-dag-bench` (rebuilds image) / `just test-dag-bench-fast`
  (skip rebuild)
- **Output:** ASCII summary table to stdout, machine-parseable `[DAG_BENCH]`
  lines to stderr, JSON export to `target/dag_bench_results/<timestamp>.json`

### 2.2 Measurement Modes

**Latency benchmarks (auto-refresh):**
- Enable the scheduler with `scheduler_interval_ms = 200` and
  `min_schedule_seconds = 1`
- INSERT into source table, start `Instant::now()` timer
- Poll `pgt_refresh_history` (100 ms interval) until the **leaf ST** has a
  new `COMPLETED` entry
- Record propagation latency = elapsed time from INSERT to leaf completion
- Also query `pgt_refresh_history` for per-ST start/end times to compute
  per-hop latency breakdown

**Throughput benchmarks (manual refresh):**
- Disable the scheduler (`pg_trickle.enabled = off`) to prevent interference
- INSERT delta into source table
- Manually `refresh_st_with_retry()` through the entire DAG in topological
  order, measuring total wall-clock time
- Compute throughput = `delta_rows × dag_depth / total_time`
- More deterministic than auto-refresh; isolates pure refresh cost from
  scheduler overhead

### 2.3 Scale

Medium scale (20–100 STs), targeting < 5 min per topology benchmark. The
source table base size is kept at 10K rows — the goal is to measure DAG
propagation overhead, not per-ST query cost (already covered by
`e2e_bench_tests.rs`).

### 2.4 Query Types Per ST

To isolate DAG overhead from query complexity, STs use **simple, cheap
queries** — lightweight aggregates (SUM/COUNT with GROUP BY) and filters
(WHERE on indexed column). This keeps per-ST $T_r$ in the 5–20 ms range so
that scheduler overhead and DAG traversal dominate the measurements.

For ST-on-ST layers, queries follow the existing patterns from
`e2e_multi_cycle_dag_tests.rs`:
- L1 (base→ST): `SELECT grp, SUM(val), COUNT(*) FROM src GROUP BY grp`
- L2+ (ST→ST): `SELECT grp, total * 2 AS doubled FROM prev_st` or
  `SELECT grp, total FROM prev_st WHERE total > 0`

---

## 3. Data Structures

### 3.1 `DagBenchResult`

```rust
struct DagBenchResult {
    topology: String,          // "linear_chain", "wide_dag", "fan_out", "diamond", "mixed"
    refresh_mode: String,      // "CALCULATED" or "PARALLEL_C4" / "PARALLEL_C8"
    measurement: String,       // "latency" or "throughput"
    dag_depth: u32,
    dag_width: u32,            // max width at any level
    total_sts: u32,
    delta_rows: u32,           // rows changed at source
    cycle: usize,
    propagation_ms: f64,       // source INSERT → leaf ST updated (latency mode)
                               // or full DAG refresh wall-clock (throughput mode)
    per_hop_avg_ms: f64,       // propagation_ms / dag_depth
    throughput_rows_per_sec: f64,  // delta_rows / (propagation_ms / 1000)
    theoretical_ms: f64,       // predicted latency from PLAN_DAG_PERFORMANCE formulas
    overhead_pct: f64,         // (actual - theoretical) / theoretical × 100
    per_st_breakdown: Vec<StTimingEntry>,  // per-ST timing from pgt_refresh_history
}

struct StTimingEntry {
    pgt_name: String,
    level: u32,
    refresh_ms: f64,
    action: String,            // "DIFFERENTIAL" or "FULL"
}
```

### 3.2 `DagTopology`

Returned by every topology builder:

```rust
struct DagTopology {
    source_tables: Vec<String>,    // base table names
    all_sts: Vec<String>,          // all ST names in topological order
    leaf_sts: Vec<String>,         // terminal STs (no downstream consumers)
    depth: u32,                    // longest path from source to leaf
    max_width: u32,                // max STs at any single level
    levels: Vec<Vec<String>>,      // STs grouped by level (for parallel dispatch analysis)
}
```

---

## 4. Topology Builders

Each builder creates the source table(s), populates them, and creates all STs.
Returns a `DagTopology` for the test to use.

### 4.1 `build_linear_chain(db, depth)`

```
src → st_lc_1 → st_lc_2 → ... → st_lc_N
```

- **Source:** `lc_src(id SERIAL PK, grp TEXT, val INT)`, 10K rows, 10 groups
- **L1:** `SELECT grp, SUM(val) AS total, COUNT(*) AS cnt FROM lc_src GROUP BY grp`
- **L2+:** Alternating project (`total * 2 AS doubled`) and filter (`WHERE total > 0`)
  to keep the query cheap but non-trivial
- **Depths:** 5, 10, 20

### 4.2 `build_wide_dag(db, depth, width)`

```
src → [st_wd_1_1 .. st_wd_1_W] → [st_wd_2_1 .. st_wd_2_W] → ... → [st_wd_D_1 .. st_wd_D_W]
```

- **Source:** `wd_src(id SERIAL PK, grp TEXT, val INT)`, 10K rows
- **L1:** `width` independent STs, each with a different filter or aggregate
  partition (e.g., `WHERE grp = 'g01'`, `WHERE grp = 'g02'`, …)
- **L2+:** Each ST at level L depends on the ST directly above it (same
  position index), forming `width` parallel chains of depth `depth`
- **Configurations:** (depth=3, width=20), (depth=5, width=20), (depth=3, width=50)

### 4.3 `build_fan_out_tree(db, depth, branching_factor)`

```
src → st_fo_1 → [st_fo_2_1, st_fo_2_2] → [st_fo_3_1 .. st_fo_3_4] → ...
```

- **Source:** `fo_src(id SERIAL PK, grp TEXT, val INT)`, 10K rows
- **Each node fans out** to `branching_factor` children with different
  filter/project queries
- **Total STs:** $(b^d - 1) / (b - 1)$ where b = branching factor, d = depth
- **Configurations:** (depth=5, b=2) → 31 STs, (depth=4, b=3) → 40 STs

### 4.4 `build_diamond(db, fan_out, fan_in_depth)`

```
src → [st_dm_a, st_dm_b, st_dm_c, st_dm_d] → st_dm_join
```

- **Source:** `dm_src(id SERIAL PK, grp TEXT, val INT)`, 10K rows
- **L1:** `fan_out` independent STs, each with a different aggregate
  (SUM, COUNT, AVG, MAX, …)
- **L2:** Single ST that JOINs all L1 STs on `grp`
- **Optionally extend** with `fan_in_depth` additional layers after the join
- **Configurations:** fan_out=2, fan_out=4, fan_out=4 + 2 layers after join

### 4.5 `build_mixed(db)`

A realistic 30–50 ST topology combining chains, fan-out, and diamonds,
modeled on the e-commerce example from PLAN_DAG_PERFORMANCE.md §3.5:

```
orders ──→ orders_daily ──→ orders_summary
        ├→ orders_by_region ─┤
        └→ orders_by_product ─┘──→ exec_dashboard
products ──→ product_stats ──────→ exec_dashboard
```

- **Two source tables:** `orders(id, region, product_id, amount, ts)` and
  `products(id, name, category)`
- **~35 STs** across 4 layers: base aggregates → domain views → dashboards →
  alerts
- **Mix of:** linear chains (3 deep), fan-out (4 branches), diamond (2
  branches converging), and cross-source joins

---

## 5. Latency Benchmarks (Auto-Refresh)

All latency benchmarks use `E2eDb::new_on_postgres_db()` (scheduler-aware
constructor with process-local guard). Each test:

1. Creates the DB and extension
2. Configures the scheduler:
   ```sql
   ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = 200;
   ALTER SYSTEM SET pg_trickle.min_schedule_seconds = 1;
   ALTER SYSTEM SET pg_trickle.auto_backoff = off;
   -- For PARALLEL mode tests:
   ALTER SYSTEM SET pg_trickle.parallel_refresh_mode = 'on';
   ALTER SYSTEM SET pg_trickle.max_concurrent_refreshes = <C>;
   ```
3. Builds the topology via the builder helper
4. Waits for initial stabilization (all STs populated via
   `wait_for_auto_refresh()`)
5. Runs `CYCLES` measurement rounds:
   - Records `before_count` of COMPLETED entries in `pgt_refresh_history` for
     the leaf ST
   - `let start = Instant::now()`
   - INSERT 100 rows into source table
   - Poll `pgt_refresh_history` for leaf ST until `count > before_count`
   - `let propagation_ms = start.elapsed().as_secs_f64() * 1000.0`
   - Query per-ST timing breakdown from `pgt_refresh_history`
6. Reports results

### 5.1 Theoretical Comparison

Each latency benchmark computes the theoretical prediction from
PLAN_DAG_PERFORMANCE.md and records the delta:

| Mode | Formula |
|------|---------|
| CALCULATED | $L = I_s + N \times T_r$ |
| PARALLEL(C) | $L = \sum_{l=1}^{D} \lceil W_l / C \rceil \times \max(I_p, T_r)$ |

Where $T_r$ is the **measured** average per-ST refresh time from the
`per_st_breakdown`, $I_s = 200\text{ms}$ (configured interval), and
$I_p = 200\text{ms}$ (parallel poll interval).

### 5.2 Test Matrix

| Test Function | Topology | Mode | STs | Expected Latency |
|---------------|----------|------|-----|-----------------|
| `bench_latency_linear_5_calc` | Linear, D=5 | CALCULATED | 5 | < 1.5s |
| `bench_latency_linear_10_calc` | Linear, D=10 | CALCULATED | 10 | < 2.5s |
| `bench_latency_linear_20_calc` | Linear, D=20 | CALCULATED | 20 | < 5s |
| `bench_latency_linear_10_par4` | Linear, D=10 | PARALLEL(4) | 10 | > CALCULATED (expected worse) |
| `bench_latency_wide_3x20_calc` | Wide, D=3 W=20 | CALCULATED | 60 | < 15s |
| `bench_latency_wide_3x20_par4` | Wide, D=3 W=20 | PARALLEL(4) | 60 | ~6s |
| `bench_latency_wide_3x20_par8` | Wide, D=3 W=20 | PARALLEL(8) | 60 | ~4s |
| `bench_latency_wide_5x20_calc` | Wide, D=5 W=20 | CALCULATED | 100 | < 25s |
| `bench_latency_wide_5x20_par8` | Wide, D=5 W=20 | PARALLEL(8) | 100 | ~8s |
| `bench_latency_fanout_b2d5_calc` | Fan-out, b=2 d=5 | CALCULATED | 31 | < 8s |
| `bench_latency_fanout_b2d5_par8` | Fan-out, b=2 d=5 | PARALLEL(8) | 31 | ~3s |
| `bench_latency_diamond_4_calc` | Diamond, fan=4 | CALCULATED | 5 | < 1.5s |
| `bench_latency_mixed_calc` | Mixed, ~35 STs | CALCULATED | ~35 | < 10s |
| `bench_latency_mixed_par8` | Mixed, ~35 STs | PARALLEL(8) | ~35 | < 5s |

Total: **14 latency benchmarks**, each running `CYCLES = 5` measurement
rounds after 2 warm-up rounds. At ~30–60s each, the latency suite runs in
~10–15 min.

---

## 6. Throughput Benchmarks (Manual Refresh)

Throughput benchmarks disable the scheduler and measure pure refresh cost. Each
test:

1. Creates the DB and extension with `pg_trickle.enabled = off`
2. Builds the topology via the builder helper
3. Manually refreshes the entire DAG once (warm-up / initial population)
4. For each `delta_size` in [10, 100, 1000]:
   - Apply mixed DML (70% UPDATE, 15% DELETE, 15% INSERT) to the source table
   - `let start = Instant::now()`
   - `refresh_st_with_retry()` through all STs in topological order
   - `let total_ms = start.elapsed().as_secs_f64() * 1000.0`
   - Record `throughput_rows_per_sec = delta_size / (total_ms / 1000.0)`
5. Repeat for `CYCLES = 5` measurement rounds

### 6.1 Test Matrix

| Test Function | Topology | STs | Delta Sizes |
|---------------|----------|-----|-------------|
| `bench_throughput_linear_5` | Linear, D=5 | 5 | 10, 100, 1000 |
| `bench_throughput_linear_10` | Linear, D=10 | 10 | 10, 100, 1000 |
| `bench_throughput_linear_20` | Linear, D=20 | 20 | 10, 100, 1000 |
| `bench_throughput_wide_3x20` | Wide, D=3 W=20 | 60 | 10, 100, 1000 |
| `bench_throughput_fanout_b2d5` | Fan-out, b=2 d=5 | 31 | 10, 100, 1000 |
| `bench_throughput_diamond_4` | Diamond, fan=4 | 5 | 10, 100, 1000 |
| `bench_throughput_mixed` | Mixed, ~35 STs | ~35 | 10, 100, 1000 |

Total: **7 throughput benchmarks** × 3 delta sizes × 5 cycles = 105
measurements. Estimated ~5–10 min.

---

## 7. Per-ST Timing Breakdown

After each measurement cycle, query `pgt_refresh_history` for per-ST timing
within the most recent cycle:

```sql
SELECT
    st.pgt_name,
    h.action,
    EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000.0 AS refresh_ms,
    h.rows_inserted + h.rows_deleted AS delta_rows
FROM pgtrickle.pgt_refresh_history h
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
WHERE h.status = 'COMPLETED'
  AND h.start_time >= $1  -- cycle start timestamp
ORDER BY h.start_time ASC;
```

This produces the `per_st_breakdown` field in `DagBenchResult`, enabling:
- Per-level latency analysis (group STs by their DAG level)
- Per-hop cost validation ($T_r$ as used in the formulas)
- Detection of unexpected FULL fallbacks in ST-on-ST chains
- Delta amplification detection (delta_rows growing across levels)

---

## 8. Reporting

### 8.1 ASCII Summary Table

Printed to stdout after each test, matching the style of `e2e_bench_tests.rs`:

```
╔══════════════════════════════════════════════════════════════════════════════════════════════════════╗
║                         pg_trickle DAG Topology Benchmark Results                                 ║
╠═══════════════╤═══════════════╤══════╤═══════╤═══════╤════════════╤════════════╤═══════════════════╣
║ Topology      │ Mode          │ STs  │ Depth │ Width │ Actual ms  │ Theory ms  │ Overhead          ║
╠═══════════════╪═══════════════╪══════╪═══════╪═══════╪════════════╪════════════╪═══════════════════╣
║ linear_chain  │ CALCULATED    │   10 │    10 │     1 │      820.3 │      700.0 │ +17.2%            ║
║ wide_dag      │ PARALLEL_C8   │   60 │     3 │    20 │     2430.1 │     1800.0 │ +35.0%            ║
║ ...           │               │      │       │       │            │            │                   ║
╚═══════════════╧═══════════════╧══════╧═══════╧═══════╧════════════╧════════════╧═══════════════════╝
```

### 8.2 Machine-Parseable Lines

Emitted to stderr for CI tooling:

```
[DAG_BENCH] topology=linear_chain mode=CALCULATED sts=10 depth=10 width=1 cycle=1 actual_ms=820.3 theory_ms=700.0 overhead_pct=17.2 per_hop_ms=82.0
```

### 8.3 JSON Export

Written to `target/dag_bench_results/<timestamp>.json` (directory created
automatically). Structure mirrors `DagBenchResult` fields. Directory
overridable via `PGS_DAG_BENCH_JSON_DIR` env var.

### 8.4 Per-Level Breakdown

For latency benchmarks, print a per-level timing table:

```
  Per-Level Breakdown (linear_chain D=10, CALCULATED):
  Level  1: avg  52.3ms  [st_lc_1]
  Level  2: avg  48.7ms  [st_lc_2]
  ...
  Level 10: avg  51.2ms  [st_lc_10]
  Total:       513.5ms  (scheduler overhead: 306.8ms)
```

---

## 9. Helper Functions

### 9.1 Topology Builders

Located in `tests/e2e_dag_bench_tests.rs` as module-private `async fn`:

```rust
async fn build_linear_chain(db: &E2eDb, depth: u32) -> DagTopology { ... }
async fn build_wide_dag(db: &E2eDb, depth: u32, width: u32) -> DagTopology { ... }
async fn build_fan_out_tree(db: &E2eDb, depth: u32, branching_factor: u32) -> DagTopology { ... }
async fn build_diamond(db: &E2eDb, fan_out: u32, extra_depth: u32) -> DagTopology { ... }
async fn build_mixed(db: &E2eDb) -> DagTopology { ... }
```

Each builder:
1. Creates the source table(s) and populates them with 10K rows
2. Runs `ANALYZE` on the source table
3. Creates all STs in topological order using `db.create_st()`
4. Returns `DagTopology` with metadata

### 9.2 Measurement Drivers

```rust
/// Run a latency benchmark: enable scheduler, INSERT into source, measure
/// propagation time to leaf ST via pgt_refresh_history polling.
async fn measure_latency(
    db: &E2eDb,
    topo: &DagTopology,
    delta_rows: u32,
    cycles: usize,
    warmup: usize,
) -> Vec<DagBenchResult> { ... }

/// Run a throughput benchmark: disable scheduler, manual topological refresh,
/// measure wall-clock time for full DAG refresh.
async fn measure_throughput(
    db: &E2eDb,
    topo: &DagTopology,
    delta_sizes: &[u32],
    cycles: usize,
) -> Vec<DagBenchResult> { ... }
```

### 9.3 Timing Collection

```rust
/// Query pgt_refresh_history for per-ST timing since `since_ts`.
async fn collect_per_st_timing(
    db: &E2eDb,
    since_ts: &str,
) -> Vec<StTimingEntry> { ... }

/// Wait until the leaf ST has a new COMPLETED refresh entry.
/// Returns the elapsed time in ms.
async fn wait_for_leaf_refresh(
    db: &E2eDb,
    leaf_st: &str,
    before_count: i64,
    timeout: Duration,
) -> Option<f64> { ... }
```

### 9.4 Theoretical Prediction

```rust
/// Compute theoretical latency from PLAN_DAG_PERFORMANCE.md formulas.
fn theoretical_latency_ms(
    mode: &str,
    levels: &[Vec<String>],
    avg_tr_ms: f64,
    scheduler_interval_ms: f64,
    poll_interval_ms: f64,
    concurrency: u32,
) -> f64 {
    match mode {
        "CALCULATED" => scheduler_interval_ms + levels.iter().map(|l| l.len()).sum::<usize>() as f64 * avg_tr_ms,
        m if m.starts_with("PARALLEL") => {
            levels.iter().map(|level| {
                let batches = (level.len() as f64 / concurrency as f64).ceil();
                batches * poll_interval_ms.max(avg_tr_ms)
            }).sum()
        }
        _ => 0.0,
    }
}
```

### 9.5 DML Generation

```rust
/// Apply a mixed DML workload to the source table.
/// Mix: 70% UPDATE, 15% DELETE, 15% INSERT.
async fn apply_dml_mix(db: &E2eDb, source_table: &str, delta_size: u32) { ... }
```

Reuses the same mix ratios as `e2e_bench_tests.rs` for consistency:
- UPDATE: `UPDATE {table} SET val = val + 1 WHERE id IN (SELECT id FROM {table} ORDER BY random() LIMIT {n})`
- DELETE: `DELETE FROM {table} WHERE id IN (SELECT id FROM {table} ORDER BY random() LIMIT {n})`
- INSERT: `INSERT INTO {table} (grp, val) SELECT 'g' || (random()*9)::int, (random()*1000)::int FROM generate_series(1, {n})`

---

## 10. Scheduler Configuration

### 10.1 Latency Benchmarks

```rust
async fn configure_latency_scheduler(db: &E2eDb, mode: &str, concurrency: u32) {
    db.alter_system_set_and_wait(
        "pg_trickle.scheduler_interval_ms", "200", "200"
    ).await;
    db.alter_system_set_and_wait(
        "pg_trickle.min_schedule_seconds", "1", "1"
    ).await;
    db.alter_system_set_and_wait(
        "pg_trickle.auto_backoff", "off", "off"
    ).await;

    if mode.starts_with("PARALLEL") {
        db.alter_system_set_and_wait(
            "pg_trickle.parallel_refresh_mode", "'on'", "on"
        ).await;
        db.alter_system_set_and_wait(
            "pg_trickle.max_concurrent_refreshes",
            &concurrency.to_string(),
            &concurrency.to_string(),
        ).await;
    }

    assert!(
        db.wait_for_scheduler(Duration::from_secs(90)).await,
        "Scheduler did not appear within 90s"
    );
}
```

### 10.2 Throughput Benchmarks

```sql
-- Disable scheduler to prevent interference
ALTER SYSTEM SET pg_trickle.enabled = off;
```

All STs use `SCHEDULE 'NONE'` (manual refresh mode) and the scheduler is
disabled server-wide.

---

## 11. Implementation Sessions

### Session 1: Infrastructure & Linear Chain (est. effort: large)

**Deliverables:**
- [ ] `tests/e2e_dag_bench_tests.rs` — file structure, imports, constants
- [ ] `DagBenchResult`, `StTimingEntry`, `DagTopology` structs
- [ ] `build_linear_chain()` topology builder
- [ ] `measure_latency()` and `measure_throughput()` drivers
- [ ] `collect_per_st_timing()`, `wait_for_leaf_refresh()` helpers
- [ ] `theoretical_latency_ms()` formula implementation
- [ ] `apply_dml_mix()` DML generator
- [ ] `print_dag_results_table()`, `write_dag_results_json()` reporting
- [ ] `configure_latency_scheduler()` helper
- [ ] `bench_latency_linear_5_calc`, `bench_latency_linear_10_calc`,
  `bench_latency_linear_20_calc`, `bench_latency_linear_10_par4` tests
- [ ] `bench_throughput_linear_5`, `bench_throughput_linear_10`,
  `bench_throughput_linear_20` tests

**Verification:**
```bash
just fmt && just lint
just build-e2e-image
cargo test --test e2e_dag_bench_tests -- --ignored bench_latency_linear_5 --test-threads=1 --nocapture
cargo test --test e2e_dag_bench_tests -- --ignored bench_throughput_linear_5 --test-threads=1 --nocapture
```

### Session 2: Wide DAG & Fan-Out (est. effort: medium)

**Deliverables:**
- [ ] `build_wide_dag()` topology builder
- [ ] `build_fan_out_tree()` topology builder
- [ ] All wide DAG latency tests (5 tests)
- [ ] All fan-out latency tests (2 tests)
- [ ] Wide DAG and fan-out throughput tests (2 tests)

**Verification:**
```bash
just fmt && just lint
cargo test --test e2e_dag_bench_tests -- --ignored bench_latency_wide --test-threads=1 --nocapture
cargo test --test e2e_dag_bench_tests -- --ignored bench_latency_fanout --test-threads=1 --nocapture
```

### Session 3: Diamond, Mixed & Reporting (est. effort: medium)

**Deliverables:**
- [ ] `build_diamond()` topology builder
- [ ] `build_mixed()` topology builder
- [ ] Diamond latency and throughput tests (2 tests)
- [ ] Mixed topology latency and throughput tests (3 tests)
- [ ] Per-level breakdown reporting
- [ ] Summary table across all topologies

**Verification:**
```bash
just fmt && just lint
cargo test --test e2e_dag_bench_tests -- --ignored bench_latency_diamond --test-threads=1 --nocapture
cargo test --test e2e_dag_bench_tests -- --ignored bench_latency_mixed --test-threads=1 --nocapture
```

### Session 4: Integration & Documentation (est. effort: small)

**Deliverables:**
- [ ] justfile targets: `test-dag-bench`, `test-dag-bench-fast`
- [ ] Update `docs/BENCHMARK.md` — add DAG Topology Benchmark section
- [ ] Full suite validation run

**Verification:**
```bash
just fmt && just lint
just test-dag-bench
# Verify JSON output in target/dag_bench_results/
# Verify ASCII summary tables with theoretical comparison
```

---

## 12. justfile Targets

```just
# Run DAG topology benchmark suite (rebuilds Docker image)
[group: "bench"]
test-dag-bench: build-e2e-image
    ./scripts/run_e2e_tests.sh --test e2e_dag_bench_tests --features pg18 --run-ignored all --no-capture

# Run DAG topology benchmarks, skip Docker image rebuild
[group: "bench"]
test-dag-bench-fast:
    ./scripts/run_e2e_tests.sh --test e2e_dag_bench_tests --features pg18 --run-ignored all --no-capture
```

---

## 13. Out of Scope

These are valuable but deferred to future work:

| Item | Reason |
|------|--------|
| **Cyclic SCC benchmarks** | Niche topology, complex fixed-point setup; defer until SCC usage is more common |
| **Scale > 100 STs** | Diminishing insight for test runtime; the medium scale captures scheduling overhead |
| **NOTIFY-based latency** | More precise than polling, but harder to implement in async tests; polling is sufficient for regression detection |
| **Cross-run comparison tool** | JSON export enables offline comparison; defer `dag_bench_compare.sh` |
| **Criterion-level DAG benchmarks** | Already partially covered in `benches/refresh_bench.rs` (topological sort) |
| **Sustained-write soak tests** | Concurrent writes + scheduler would measure steady-state throughput; defer as a separate "stress test" tier |
| **Tiered scheduling benchmarks** | `hot`/`warm`/`economy` tier comparison is a configuration knob, not a topology; defer |

---

## 14. Success Criteria

1. **All 21 benchmark tests pass** without assertion failures (benchmarks
   should not assert on specific timings — only report them).
2. **Linear chain CALCULATED mode is faster than PARALLEL** for the same
   depth — validates the poll-overhead prediction from §3.1.
3. **Wide DAG PARALLEL(C=8) shows measurable speedup over CALCULATED** for
   width ≥ 20 — validates the crossover prediction from §3.2.
4. **Theoretical vs actual overhead is < 100%** for all topologies — formulas
   should be in the right ballpark, not off by orders of magnitude.
5. **Per-ST breakdown shows DIFFERENTIAL action** for ST-on-ST hops — confirms
   v0.11.0 differential propagation is working.
6. **JSON output is parseable** and contains all `DagBenchResult` fields.
7. **`just test-dag-bench` completes in < 30 min** for the full suite.
