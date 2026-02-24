# PLAN_TRIGGERS_OVERHEAD.md — CDC Trigger Write-Side Overhead Benchmark

## 1. Motivation

The existing benchmark suite (`tests/e2e_bench_tests.rs`) measures **refresh duration** — how fast incremental refresh processes changes — but says nothing about the **write-side cost** the CDC trigger imposes on source tables. Every INSERT, UPDATE, or DELETE on a source table fires a PL/pgSQL AFTER trigger that:

1. Calls `pg_current_wal_lsn()`
2. Computes `pg_stream_hash(NEW."pk"::text)` (or `pg_stream_hash_multi(ARRAY[...])` for composite PKs)
3. Inserts a row into `pg_stream_changes.changes_<oid>` with typed `new_*`/`old_*` columns
4. Maintains the covering B-tree index `(lsn, pk_hash, change_id) INCLUDE (action)`
5. Increments the `change_id` BIGSERIAL sequence

This overhead is invisible in refresh benchmarks but directly impacts the **DML throughput** of every monitored source table. Users need data to answer: *"How much slower are my writes with a stream table watching this source?"*

### Current Trigger Function (generated per source)

For a table with OID `16384` and columns `(id INT, amount NUMERIC)` with PK on `id`:

```sql
CREATE OR REPLACE FUNCTION pg_stream_changes.pg_stream_cdc_fn_16384()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO pg_stream_changes.changes_16384
            (lsn, action, pk_hash, "new_id", "new_amount")
        VALUES (pg_current_wal_lsn(), 'I',
                pg_stream.pg_stream_hash(NEW."id"::text), NEW."id", NEW."amount");
        RETURN NEW;
    ELSIF TG_OP = 'UPDATE' THEN
        INSERT INTO pg_stream_changes.changes_16384
            (lsn, action, pk_hash, "new_id", "new_amount", "old_id", "old_amount")
        VALUES (pg_current_wal_lsn(), 'U',
                pg_stream.pg_stream_hash(NEW."id"::text), NEW."id", NEW."amount",
                OLD."id", OLD."amount");
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO pg_stream_changes.changes_16384
            (lsn, action, pk_hash, "old_id", "old_amount")
        VALUES (pg_current_wal_lsn(), 'D',
                pg_stream.pg_stream_hash(OLD."id"::text), OLD."id", OLD."amount");
        RETURN OLD;
    END IF;
    RETURN NULL;
END;
$$;
```

The trigger cost is proportional to:
- **Column count** — each column produces a `new_*` and/or `old_*` typed value in the INSERT
- **PK type** — single-column uses `pg_stream_hash()`, composite uses `pg_stream_hash_multi(ARRAY[...])`
- **DML operation** — UPDATE writes both `new_*` and `old_*` (widest buffer row)

---

## 2. Goals

- Quantify per-row trigger overhead in **µs/row** (absolute cost)
- Report **throughput ratio** (ops/sec without trigger ÷ ops/sec with trigger) where > 1.0 means trigger is slower
- Sweep across three dimensions that affect trigger cost:
  - **Column count** (controls typed-column INSERT width)
  - **PK type** (controls hash function used)
  - **DML operation** (INSERT/UPDATE/DELETE/mixed — UPDATE is worst-case)
- Establish a baseline for evaluating future trigger optimizations (e.g., UNLOGGED change buffers, column pruning, logical replication migration)

---

## 3. Benchmark Design

### 3.1. Methodology

For each (schema, pk_type, dml_operation) combination:

1. **Baseline run** — execute DML on the source table with **no trigger installed** (plain table, no stream table created). Measure wall-clock time for `BATCH_SIZE` operations across `CYCLES` iterations.

2. **Trigger run** — create a stream table referencing the source (which installs the CDC trigger automatically via `pg_stream.create_stream_table()`). TRUNCATE the change buffer between cycles to isolate per-row trigger cost from buffer-growth/bloat effects. Measure the same DML workload.

3. **Compute overhead**:
   - `overhead_us_per_row = (trigger_avg_us - baseline_avg_us) / BATCH_SIZE`
   - `throughput_ratio = baseline_ops_per_sec / trigger_ops_per_sec`
   - Also capture P95 for both runs to detect variance/outliers

### 3.2. Parameters

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `BATCH_SIZE` | 10,000 rows/cycle | Large enough to amortize per-statement overhead; matches 100K/10% change rate from refresh benchmarks |
| `CYCLES` | 10 | Consistent with `e2e_bench_tests.rs` |
| `WARMUP_CYCLES` | 2 | Discarded to eliminate buffer cache warming effects |
| `PRE_POPULATE` | 100,000 rows | For UPDATE/DELETE: ensures enough rows to operate on |
| `DML_STATEMENT` | Single multi-row statement per operation type | Uses `generate_series` for INSERT, subquery-selected random rows for UPDATE/DELETE |

### 3.3. Table Schema Fixtures

Three column widths to measure how typed-column expansion affects trigger cost:

**Narrow (3 columns):**
```sql
CREATE TABLE src_narrow (
    id    SERIAL PRIMARY KEY,
    value INT NOT NULL DEFAULT 0,
    label TEXT NOT NULL DEFAULT 'x'
);
```
Trigger INSERT has ~6 column references for UPDATE (3 `new_*` + 3 `old_*`). Buffer row ~80 bytes.

**Medium (8 columns):**
```sql
CREATE TABLE src_medium (
    id     SERIAL PRIMARY KEY,
    a      TEXT NOT NULL DEFAULT 'alpha',
    b      NUMERIC NOT NULL DEFAULT 0.0,
    c      INT NOT NULL DEFAULT 0,
    d      TIMESTAMPTZ NOT NULL DEFAULT now(),
    e      BOOLEAN NOT NULL DEFAULT false,
    f      TEXT NOT NULL DEFAULT '',
    g      NUMERIC NOT NULL DEFAULT 1.0
);
```
Trigger INSERT has ~16 column references for UPDATE (8 `new_*` + 8 `old_*`). Buffer row ~200 bytes.

**Wide (20 columns):**
```sql
CREATE TABLE src_wide (
    id    SERIAL PRIMARY KEY,
    col1  INT DEFAULT 0, col2  INT DEFAULT 0, col3  INT DEFAULT 0,
    col4  INT DEFAULT 0, col5  INT DEFAULT 0, col6  INT DEFAULT 0,
    col7  INT DEFAULT 0, col8  INT DEFAULT 0, col9  INT DEFAULT 0,
    col10 INT DEFAULT 0, col11 INT DEFAULT 0, col12 INT DEFAULT 0,
    col13 INT DEFAULT 0, col14 INT DEFAULT 0, col15 INT DEFAULT 0,
    col16 INT DEFAULT 0, col17 INT DEFAULT 0, col18 INT DEFAULT 0,
    col19 INT DEFAULT 0
);
```
Trigger INSERT has ~40 column references for UPDATE (20 `new_*` + 20 `old_*`). Buffer row ~400 bytes. This stress-tests PL/pgSQL row-decomposition and VALUES clause construction.

### 3.4. PK Type Fixtures

| PK Type | Schema Modification | Hash Function | Notes |
|---------|-------------------|---------------|-------|
| **Single INT** | `id SERIAL PRIMARY KEY` | `pg_stream_hash(NEW."id"::text)` | Baseline — cheapest hash |
| **Composite 2-col** | `PRIMARY KEY (id, seq)` with extra `seq INT NOT NULL DEFAULT 1` | `pg_stream_hash_multi(ARRAY[NEW."id"::text, NEW."seq"::text])` | Array construction + multi-hash |
| **No PK** | Remove PRIMARY KEY constraint | No `pk_hash` column in buffer | Simpler trigger, but `lsn`-only index; tests the fallback path in `src/cdc.rs` |

### 3.5. DML Operations

| Operation | Statement Pattern | Trigger Codepath |
|-----------|------------------|-----------------|
| **INSERT-only** | `INSERT INTO src SELECT ... FROM generate_series(1, N)` | `TG_OP = 'INSERT'`: writes `new_*` columns only |
| **UPDATE-only** | `UPDATE src SET value = value + 1 WHERE id IN (SELECT id FROM src ORDER BY random() LIMIT N)` | `TG_OP = 'UPDATE'`: writes both `new_*` and `old_*` columns (widest buffer row) |
| **DELETE-only** | `DELETE FROM src WHERE id IN (SELECT id FROM src ORDER BY random() LIMIT N)` | `TG_OP = 'DELETE'`: writes `old_*` columns only |
| **Mixed 70/15/15** | UPDATE 70%, DELETE 15%, INSERT 15% (same split as refresh benchmarks) | Representative production workload |

For UPDATE and DELETE cycles, the table is pre-populated with 100K rows so there are always enough rows to operate on. After DELETE cycles, rows are re-inserted to maintain pool size.

---

## 4. Implementation

### 4.1. File: `tests/e2e_trigger_overhead_tests.rs`

New test file following the established E2E pattern:

```rust
//! CDC Trigger write-side overhead benchmarks.
//!
//! Measures the per-row cost of the AFTER trigger on source tables
//! by comparing DML throughput with and without triggers installed.
//!
//! These tests are `#[ignore]`d. Run explicitly:
//!
//! ```bash
//! cargo test --test e2e_trigger_overhead_tests -- --ignored --nocapture
//! ```
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;
use e2e::E2eDb;
use std::time::Instant;
```

### 4.2. Configuration Constants

```rust
/// Rows per DML batch per cycle.
const BATCH_SIZE: usize = 10_000;

/// Number of measured cycles per combination.
const CYCLES: usize = 10;

/// Warm-up cycles discarded before measurement.
const WARMUP_CYCLES: usize = 2;

/// Pre-populated rows for UPDATE/DELETE workloads.
const PRE_POPULATE: usize = 100_000;
```

### 4.3. Core Helper: `time_dml_batch()`

```rust
/// Execute a DML workload and return per-row timing.
///
/// Returns (avg_us_per_row, p95_us_per_row, ops_per_sec, raw_cycle_times_ms).
async fn time_dml_batch(
    db: &E2eDb,
    dml_stmts: &[String],
    batch_size: usize,
    cycles: usize,
    warmup_cycles: usize,
) -> (f64, f64, f64, Vec<f64>) {
    let mut times_ms = Vec::with_capacity(cycles);

    for cycle in 0..(warmup_cycles + cycles) {
        let start = Instant::now();
        for stmt in dml_stmts {
            db.execute(stmt).await;
        }
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        if cycle >= warmup_cycles {
            times_ms.push(elapsed_ms);
        }
    }

    let avg_ms = times_ms.iter().sum::<f64>() / times_ms.len() as f64;
    let avg_us_per_row = (avg_ms * 1000.0) / batch_size as f64;
    let ops_per_sec = batch_size as f64 / (avg_ms / 1000.0);

    let mut sorted = times_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95_ms = percentile(&sorted, 95.0);
    let p95_us_per_row = (p95_ms * 1000.0) / batch_size as f64;

    (avg_us_per_row, p95_us_per_row, ops_per_sec, times_ms)
}
```

### 4.4. Core Helper: `time_dml_batch_with_cleanup()`

Variant that TRUNCATEs the change buffer between cycles to isolate per-row trigger cost:

```rust
/// Like time_dml_batch but TRUNCATEs change buffer after each cycle.
async fn time_dml_batch_with_cleanup(
    db: &E2eDb,
    dml_stmts: &[String],
    batch_size: usize,
    cycles: usize,
    warmup_cycles: usize,
    truncate_stmt: &str,
) -> (f64, f64, f64, Vec<f64>) {
    let mut times_ms = Vec::with_capacity(cycles);

    for cycle in 0..(warmup_cycles + cycles) {
        let start = Instant::now();
        for stmt in dml_stmts {
            db.execute(stmt).await;
        }
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Cleanup outside measurement window
        db.execute(truncate_stmt).await;

        if cycle >= warmup_cycles {
            times_ms.push(elapsed_ms);
        }
    }

    // ... same statistics computation as time_dml_batch
}
```

### 4.5. Core Helper: `bench_trigger_overhead()`

```rust
/// Run baseline (no trigger) and trigger (with ST) DML, compute overhead.
async fn bench_trigger_overhead(
    table_name: &str,
    create_table_sql: &str,
    populate_sql: &str,
    dml_fn: fn(usize) -> Vec<String>,
    st_query: &str,
    batch_size: usize,
) -> TriggerOverheadResult {
    // ── Phase 1: Baseline (no trigger) ──
    let db = E2eDb::new_bench().await.with_extension().await;
    db.execute(create_table_sql).await;
    if !populate_sql.is_empty() {
        db.execute(populate_sql).await;
    }

    let (base_us, base_p95, base_ops, _) =
        time_dml_batch(&db, &dml_fn(batch_size), batch_size, CYCLES, WARMUP_CYCLES)
            .await;

    // ── Phase 2: With trigger (create ST to auto-install it) ──
    // Drop and recreate table to reset bloat/stats
    db.execute(&format!("DROP TABLE IF EXISTS {} CASCADE", table_name)).await;
    db.execute(create_table_sql).await;
    if !populate_sql.is_empty() {
        db.execute(populate_sql).await;
    }

    // Creating the ST installs the CDC trigger automatically
    db.execute(&format!(
        "SELECT pg_stream.create_stream_table('overhead_st', $q${}$q$, '1 hour', 'INCREMENTAL')",
        st_query
    )).await;

    // Full initial refresh so the ST is populated
    db.execute(
        "SELECT pg_stream.refresh_stream_table('overhead_st', force_full => true)"
    ).await;

    // Discover the change buffer table name
    let change_table = get_change_table_name(&db, table_name).await;

    let (trig_us, trig_p95, trig_ops, _) =
        time_dml_batch_with_cleanup(
            &db, &dml_fn(batch_size), batch_size, CYCLES, WARMUP_CYCLES,
            &format!("TRUNCATE {}", change_table),
        ).await;

    TriggerOverheadResult {
        overhead_us_per_row: trig_us - base_us,
        throughput_ratio: base_ops / trig_ops,
        baseline_us_per_row: base_us,
        trigger_us_per_row: trig_us,
        baseline_p95_us: base_p95,
        trigger_p95_us: trig_p95,
        baseline_ops_per_sec: base_ops,
        trigger_ops_per_sec: trig_ops,
    }
}
```

### 4.6. Result Struct and Reporting

```rust
struct TriggerOverheadResult {
    overhead_us_per_row: f64,
    throughput_ratio: f64,       // > 1.0 means trigger is slower
    baseline_us_per_row: f64,
    trigger_us_per_row: f64,
    baseline_p95_us: f64,
    trigger_p95_us: f64,
    baseline_ops_per_sec: f64,
    trigger_ops_per_sec: f64,
}
```

Output format (printed to stdout with `[BENCH_TRIGGER]` prefix for parseability):

```
╔══════════════════════════════════════════════════════════════════════════════════════════════╗
║                    pg_stream Trigger Overhead Results                               ║
╠════════════╤══════════╤══════════╤══════════╤══════════╤══════════╤════════╤════════════════╣
║ Schema     │ PK Type  │ DML Op   │ Base µs  │ Trig µs  │ Δ µs/row │ Ratio  │ Trig ops/s     ║
╠════════════╪══════════╪══════════╪══════════╪══════════╪══════════╪════════╪════════════════╣
║ narrow     │ single   │ INSERT   │     1.2  │     2.8  │     1.6  │  2.3x  │     357,142    ║
║ narrow     │ single   │ UPDATE   │     2.1  │     4.5  │     2.4  │  2.1x  │     222,222    ║
║ narrow     │ single   │ DELETE   │     1.8  │     3.5  │     1.7  │  1.9x  │     285,714    ║
║ narrow     │ single   │ MIXED    │     1.9  │     4.0  │     2.1  │  2.1x  │     250,000    ║
║ medium     │ single   │ UPDATE   │     3.0  │     7.2  │     4.2  │  2.4x  │     138,888    ║
║ wide       │ single   │ UPDATE   │     4.5  │    13.0  │     8.5  │  2.9x  │      76,923    ║
║ ...        │          │          │          │          │          │        │                ║
╚════════════╧══════════╧══════════╧══════════╧══════════╧══════════╧════════╧════════════════╝
```

(Values above are hypothetical — to be replaced with actual measurements.)

### 4.7. Test Functions

All tests are `#[ignore]` and use `new_bench()` for resource-constrained containers:

```rust
/// Canary test: narrow schema, single INT PK, all 4 DML types.
/// Fastest to run (~2 min). Use this to validate the harness.
#[tokio::test]
#[ignore]
async fn bench_trigger_overhead_narrow_single_pk() { ... }

/// Column count sweep: narrow × medium × wide, UPDATE-only (worst-case).
/// Answers: "How much does column count affect trigger cost?"
#[tokio::test]
#[ignore]
async fn bench_trigger_overhead_column_count_sweep() { ... }

/// PK type sweep: single × composite × no-pk, mixed DML.
/// Answers: "How much does PK hash cost matter?"
#[tokio::test]
#[ignore]
async fn bench_trigger_overhead_pk_type_sweep() { ... }

/// Full matrix: 3 schemas × 3 PK types × 4 DML ops = 36 combinations.
/// Complete dataset. Run time ~30 min.
#[tokio::test]
#[ignore]
async fn bench_trigger_overhead_full_matrix() { ... }
```

### 4.8. Justfile Target

Add to the Benchmarks section of `justfile`:

```just
# Run trigger overhead benchmarks (requires E2E Docker image)
bench-trigger:
    cargo test --test e2e_trigger_overhead_tests -- --ignored --nocapture --test-threads=1
```

---

## 5. Sweep Matrix

### 5.1. Column Count Sweep (PK=single INT, DML=UPDATE)

| Schema | Columns | `new_*` cols | `old_*` cols | Buffer row width (est.) |
|--------|---------|-------------|-------------|------------------------|
| narrow | 3       | 3           | 3           | ~80 bytes              |
| medium | 8       | 8           | 8           | ~200 bytes             |
| wide   | 20      | 20          | 20          | ~400 bytes             |

**Hypothesis:** Trigger cost scales roughly linearly with column count because:
- PL/pgSQL must decompose `NEW` and `OLD` records into individual column references
- The `INSERT INTO changes_<oid>` VALUES clause grows linearly
- B-tree index page splits become more frequent with wider rows (higher buffer growth rate)
- WAL volume per trigger fire increases proportionally

### 5.2. PK Type Sweep (Schema=narrow, DML=mixed)

| PK Type | Hash Call | Expected overhead |
|---------|-----------|------------------|
| Single INT | `pg_stream_hash(NEW."id"::text)` | Baseline — single `::text` cast + xxh64 |
| Composite (id, seq) | `pg_stream_hash_multi(ARRAY[NEW."id"::text, NEW."seq"::text])` | Array construction + multi-element hash |
| No PK | (none — `pk_hash` column omitted) | Cheaper trigger, but index is `(lsn)` only |

**Hypothesis:** Composite PK adds ~0.5–1 µs/row over single PK due to array construction. No-PK should be slightly cheaper than single PK (no hash computation), but the `lsn`-only index may lead to wider scan ranges during refresh (not measured here, but worth noting).

### 5.3. DML Operation Sweep (Schema=narrow, PK=single)

| Operation | Trigger columns written | Expected cost |
|-----------|------------------------|---------------|
| INSERT | `new_*` only (3 cols narrow) | Cheapest — smallest buffer row |
| UPDATE | `new_*` + `old_*` (6 cols narrow) | Most expensive — widest buffer row + 2 record decompositions |
| DELETE | `old_*` only (3 cols narrow) | Same width as INSERT |
| Mixed | 70% U + 15% D + 15% I | Weighted average |

**Hypothesis:** UPDATE overhead is 1.5–2x INSERT overhead due to double column writes and both `NEW` and `OLD` record access.

---

## 6. Expected Results (Estimates)

Based on typical PL/pgSQL AFTER trigger overhead in PostgreSQL:

| Schema | PK | DML | Est. overhead µs/row | Est. ratio |
|--------|----|-----|---------------------|------------|
| narrow | single | INSERT | 1–3 | 1.5–2.5x |
| narrow | single | UPDATE | 2–5 | 2.0–3.0x |
| narrow | single | DELETE | 1–3 | 1.5–2.5x |
| narrow | single | MIXED  | 2–4 | 2.0–3.0x |
| medium | single | UPDATE | 3–8 | 2.5–4.0x |
| wide   | single | UPDATE | 5–15 | 3.0–5.0x |
| narrow | composite | MIXED | 2–4 | 2.0–3.0x |
| narrow | no-pk | MIXED | 1–2 | 1.3–2.0x |

These are rough estimates. The benchmark will provide actual numbers for this specific trigger implementation (typed columns, single covering index, BIGSERIAL sequence).

### Cost Breakdown (Estimated Per-Row)

| Component | Est. µs | Notes |
|-----------|---------|-------|
| PL/pgSQL function entry/exit | 0.5–1.0 | Fixed overhead per trigger invocation |
| `pg_current_wal_lsn()` call | 0.1–0.2 | Lightweight system function |
| `pg_stream_hash(pk::text)` | 0.2–0.5 | Cast + xxh64 hash |
| `INSERT INTO changes_<oid>` (heap) | 0.5–1.0 | Scales with row width |
| B-tree index update | 0.3–0.8 | Single covering index (was 2 previously) |
| WAL write for buffer row | 0.3–0.5 | Scales with row width |
| BIGSERIAL sequence increment | 0.1–0.3 | Shared sequence lock |
| **Total (narrow/INSERT)** | **~2–4** | |
| **Total (wide/UPDATE)** | **~5–15** | 2x column writes + wider rows |

---

## 7. Actionable Insights This Benchmark Enables

### 7.1. Decision: When to Use UNLOGGED Change Buffers

If the overhead is > 5 µs/row for wide tables, an `UNLOGGED` change buffer would eliminate WAL generation for trigger writes, potentially halving the overhead. Trade-off: change buffer data is lost on crash (acceptable if refreshes re-initialize from a full scan).

**Action:** If measured WAL overhead > 30% of trigger cost → implement `pg_stream.change_buffer_unlogged` GUC.

### 7.2. Decision: When to Migrate to Logical Replication

Per `AGENTS.md` and ADR-001/ADR-002 in `plans/adrs/PLAN_ADRS.md`, triggers are recommended for < 1,000 writes/sec and logical replication for > 5,000 writes/sec. This benchmark provides **actual per-row cost** to compute the crossover point:

- If trigger overhead = 3 µs/row → max sustained throughput ≈ 333K rows/sec (probably fine for most workloads)
- If trigger overhead = 10 µs/row → max sustained throughput ≈ 100K rows/sec (start considering alternatives at 50K+ rows/sec)

**Action:** Update the recommendation thresholds in `AGENTS.md` with measured data.

### 7.3. Decision: Column Pruning for Change Buffers

If the column-count sweep shows strong scaling (e.g., 3x overhead at 20 cols vs 3 cols), then a future optimization could prune the change buffer to only capture columns actually referenced in the stream table's defining query (via `columns_used` in `pg_stream.pgs_dependencies`).

**Action:** If wide-table overhead > 3x narrow-table overhead → prioritize column pruning optimization.

### 7.4. Decision: Sequence Contention

The `change_id BIGSERIAL` increments a shared sequence under lock for every trigger fire. Under concurrent writers, this could become a bottleneck. While concurrent writers are out of scope for this initial benchmark, the single-writer results establish a baseline.

**Action:** If `change_id` sequence overhead is measurable → investigate replacing with `ctid`-based ordering or removing the column entirely.

---

## 8. Future Extensions (Not in Scope)

| Extension | Why Deferred |
|-----------|-------------|
| **Concurrent writers (1/4/8 connections)** | Requires pgbench-style parallel harness; adds complexity. Worth a follow-up once we see if single-writer overhead is concerning. |
| **Batch size sweep (1/100/1000 rows per txn)** | Tests per-txn vs per-row amortization. Deferred for simplicity — the 10K batch already represents bulk DML. |
| **UNLOGGED change buffer variant** | Requires code change to `create_change_buffer_table()` in `src/cdc.rs`. Benchmark first, optimize second. |
| **Trigger vs. logical replication direct comparison** | Requires `wal_level=logical` and replication slot setup. Separate benchmark once/if logical replication is implemented. |
| **Multiple stream tables per source** | Tests whether 2+ STs on the same source multiply trigger cost (they share a single trigger/buffer, so overhead should be constant). |

---

## 9. Relationship to Existing Performance Work

This benchmark fills a gap identified in `PLAN_PERFORMANCE_PART_7.md` §4.6 ("Change buffer write amplification"):

> Each source table DML triggers a per-row AFTER trigger that inserts into the change buffer. At high write rates, the change buffer itself becomes a write bottleneck due to:
> - WAL generation for every change buffer INSERT
> - Index maintenance for ~~2 indexes~~ 1 covering index (after Session 5/AA1)
> - BIGSERIAL contention on the change_id sequence

Session 5 of Part 7 (already completed) reduced the index count from 2 to 1 (single covering index `(lsn, pk_hash, change_id) INCLUDE (action)`), which should yield ~20% trigger overhead reduction. This benchmark will **measure the actual impact** by providing the first concrete trigger overhead numbers.

---

## 10. Files to Create/Modify

| Action | File | Description |
|--------|------|-------------|
| **Create** | `tests/e2e_trigger_overhead_tests.rs` | Benchmark test file with all test functions and helpers |
| **Modify** | `justfile` | Add `bench-trigger` recipe in the Benchmarks section |
| **Modify** | `BENCHMARK.md` | Add "Trigger Overhead" results section after running |

---

## 11. Running the Benchmark

```bash
# Prerequisites
./tests/build_e2e_image.sh

# Run just the canary test (~2 min)
cargo test --test e2e_trigger_overhead_tests bench_trigger_overhead_narrow_single_pk \
    -- --ignored --nocapture

# Run column-count sweep (~5 min)
cargo test --test e2e_trigger_overhead_tests bench_trigger_overhead_column_count_sweep \
    -- --ignored --nocapture

# Run PK-type sweep (~5 min)
cargo test --test e2e_trigger_overhead_tests bench_trigger_overhead_pk_type_sweep \
    -- --ignored --nocapture

# Run full matrix (~30 min, 36 combinations)
cargo test --test e2e_trigger_overhead_tests bench_trigger_overhead_full_matrix \
    -- --ignored --nocapture

# Or use the justfile shortcut (runs all)
just bench-trigger
```

---

## 12. Git Commit

After creating the implementation:

```bash
git add tests/e2e_trigger_overhead_tests.rs justfile PLAN_TRIGGERS_OVERHEAD.md
git commit -m "bench: add CDC trigger write-side overhead benchmarks

Measures per-row trigger cost across column count (3/8/20),
PK type (single/composite/none), and DML operation (I/U/D/mixed).
Reports both µs/row overhead and throughput ratio vs baseline.

New file: tests/e2e_trigger_overhead_tests.rs
New justfile target: bench-trigger"
```
