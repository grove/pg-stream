# Plan: Bootstrap Gating for External Source Population

Date: 2026-03-04
Status: IMPLEMENTED
Last Updated: 2026-03-14

---

## 1. Problem Statement

### Initial Population Creates Unnecessary Churn

When external ETL processes initially populate source tables, the pg_trickle
scheduler sees changes arriving and eagerly refreshes downstream stream
tables. If two (or more) source tables feed into a shared downstream ST and
their ETL processes run independently, the scheduler will:

1. Detect changes in the first source and refresh the downstream ST —
   joining partially-loaded data from source A with an empty source B.
2. Repeat on every scheduler tick as batches land — producing many
   intermediate results, all incomplete.
3. Only converge to the correct result after **both** ETL processes finish.

This wastes compute and produces misleading intermediate states that may be
visible to downstream consumers.

### Motivating Example

An analytics platform bootstraps two tables from an external data warehouse:

```
ETL-A: COPY orders FROM 's3://…'          (takes 10 min)
ETL-B: COPY order_lines FROM 's3://…'     (takes 20 min)
```

The DAG:

```
orders ──→ order_summary ──┐
                           ├──→ order_report
order_lines ──→ line_summary ──┘
```

Without bootstrap gating:

| Time   | Event | Result |
|--------|-------|--------|
| T+0    | ETL-A starts loading `orders` | — |
| T+1m   | Scheduler tick — `orders` has changes, `order_lines` empty | `order_summary` refreshed (partial), `line_summary` empty, `order_report` has orders without lines |
| T+2m   | More `orders` batches land | Another churn cycle |
| T+10m  | ETL-A finishes | `order_summary` nearly correct, `line_summary` still empty |
| T+11m  | ETL-B first batches | `order_report` joins full orders with partial lines |
| T+20m  | ETL-B finishes | Finally correct |

Over 20 minutes the scheduler has executed ~20 unnecessary refresh cycles,
each producing incomplete results.

### Why Existing Mechanisms Don't Help

| Mechanism | Why insufficient |
|-----------|-----------------|
| **`initialize => false`** | Prevents initial population of the ST itself, but doesn't prevent the scheduler from refreshing it once changes appear in sources. Also requires the user to manually trigger initialization later. |
| **Diamond consistency** | No shared PostgreSQL ancestor — the sources are independent base tables. |
| **Cross-source snapshot** | Ensures PG-level snapshot coherence, but external temporal skew is already baked into the base table contents. |
| **Watermark gating** ([PLAN_WATERMARK_GATING.md](PLAN_WATERMARK_GATING.md)) | Addresses *ongoing* temporal alignment of external sources. Bootstrap is a one-time readiness problem — simpler, but related. See §7 for how they compose. |

### Core Insight

The scheduler needs a **gate** that prevents downstream processing until all
required sources have completed their initial population. Only the external
process knows when initial load is complete. The gate should:

- Be **configurable** — not all source tables need bootstrap gating.
- Be **temporary** — once all sources are ready, it lifts permanently.
- Be **simple** — binary ready/not-ready per source, no temporal algebra.
- **Compose** naturally with watermark gating for the steady-state case.

---

## 2. Design Options

### 2.1 Option A — Explicit `mark_source_ready()` Function

The external ETL process calls a SQL function after the initial load:

```sql
-- ETL-A finishes initial population
BEGIN;
  COPY orders FROM 's3://warehouse/orders.parquet';
  SELECT pgtrickle.mark_source_ready('orders');
COMMIT;
```

The scheduler checks: for each ST, if any transitive source has a bootstrap
gate configured but has not been marked ready, skip the ST.

**Configuration** — at the source level:

```sql
-- Before ETL starts, declare that these sources need bootstrap gating
SELECT pgtrickle.set_bootstrap_gate('orders');
SELECT pgtrickle.set_bootstrap_gate('order_lines');

-- Or at stream table creation time
SELECT pgtrickle.create_stream_table('order_report', '...', '1m',
    await_sources => ARRAY['orders', 'order_lines']
);
```

**Readiness signal semantics:**

- **Transactional:** `mark_source_ready()` is part of the caller's
  transaction. The readiness signal becomes visible only when the data
  does — preventing the scheduler from seeing "ready" before the data
  commits.
- **Idempotent:** Calling `mark_source_ready()` on an already-ready source
  is a no-op.
- **Permanent:** Once marked ready, the gate is lifted for the lifetime of
  the source's participation in the DAG. The gate can be re-enabled via
  `set_bootstrap_gate()` for a re-bootstrap scenario.
- **Signal:** Notifies the scheduler (via `pg_notify` or shared-memory
  signal) so gating is re-evaluated on the next tick.

### 2.2 Option B — Compose with Watermark Gating

Treat bootstrap as a special case of watermarks: a source in a watermark
group whose watermark is `NULL` (never advanced) is considered "not ready."

The current watermark gating plan (§9, question #9) proposes that `NULL`
watermarks mean "ungated." This option **inverts** that default for sources
that are members of a watermark group: `NULL` = gated, first
`advance_watermark()` = ready.

```sql
-- Create watermark group — gating is active immediately
SELECT pgtrickle.create_watermark_group('order_pipeline',
    sources   => ARRAY['orders', 'order_lines'],
    tolerance => '0 seconds'
);

-- ETL-A finishes, advances watermark for the first time
SELECT pgtrickle.advance_watermark('orders', '2026-03-01');

-- ETL-B finishes, advances watermark for the first time
SELECT pgtrickle.advance_watermark('order_lines', '2026-03-01');

-- Both sources now have non-NULL watermarks → alignment evaluates → STs refresh
```

Bootstrap naturally transitions to steady-state temporal alignment — no
separate API needed.

### 2.3 Option C — `initialize => false` + Deferred Initialization

Extend the existing `initialize` parameter with an `await_sources` list.
The ST is created but not scheduled until all listed sources signal readiness:

```sql
SELECT pgtrickle.create_stream_table('order_report', '...', '1m',
    initialize => false,
    await_sources => ARRAY['orders', 'order_lines']
);
```

When all awaited sources are marked ready, the scheduler auto-initializes
the ST (runs the initial full refresh) and begins normal scheduling.

### 2.4 Option D — Source-Level DAG Gating

Model readiness as a property of source nodes in the DAG. A gated source
blocks all transitive downstream STs from refreshing.

```sql
-- Register sources as gated before ETL starts
SELECT pgtrickle.gate_source('orders');
SELECT pgtrickle.gate_source('order_lines');

-- ETL runs...

-- Signal completion
SELECT pgtrickle.ungate_source('orders');
SELECT pgtrickle.ungate_source('order_lines');
```

Any ST with a transitive dependency on a gated source is automatically
skipped. No per-ST configuration needed — gating flows through the DAG.

---

## 3. Comparison

| | **A — mark_source_ready()** | **B — Watermark composition** | **C — Deferred init** | **D — DAG source gating** |
|---|---|---|---|---|
| **API surface** | New: `set_bootstrap_gate()`, `mark_source_ready()` | Reuses watermark API | Extends existing `create_stream_table` | New: `gate_source()`, `ungate_source()` |
| **Configuration granularity** | Per-source or per-ST (`await_sources`) | Per watermark group | Per-ST | Per-source (DAG propagation) |
| **Ongoing overhead** | None after bootstrap | Full watermark evaluation on every tick | None after init | None after ungating |
| **Composes with watermarks** | Orthogonal — both can apply | Native composition | Orthogonal | Orthogonal — subsumable by watermarks |
| **Requires watermark infra** | No | Yes | No | No |
| **Handles re-bootstrap** | Yes (`set_bootstrap_gate()` re-arms) | Yes (reset watermark to NULL — needs new API) | Partially (`needs_reinit` + re-create) | Yes (`gate_source()` re-arms) |
| **Intermediate ST behavior** | Configurable (per-ST `await_sources`) | Depends on watermark gating mode (§5 of watermark plan) | Only the ST with `await_sources` is gated | All transitive descendants blocked |
| **Implementation complexity** | Low | Medium (depends on watermark plan) | Low | Low-Medium |

---

## 4. Recommendation

### Phased Approach

**Phase 1 — Option D (Source-Level DAG Gating):** Implement as a
standalone, low-overhead feature. It's the simplest model with the strongest
automatic propagation — gating a source automatically gates everything
downstream without per-ST configuration.

The API is two functions:

```sql
-- Gate a source (blocks all downstream STs)
SELECT pgtrickle.gate_source('orders');

-- Ungate a source (allows downstream processing)
SELECT pgtrickle.ungate_source('orders');
```

Plus an introspection function:

```sql
-- Show current gate status for all tracked sources
SELECT * FROM pgtrickle.source_gates();
```

**Phase 2 — Watermark Subsumption (Option B):** When the watermark gating
plan ([PLAN_WATERMARK_GATING.md](PLAN_WATERMARK_GATING.md)) is implemented,
bootstrap gating becomes a natural subset: a source with no watermark
(`NULL`) in a watermark group is effectively gated. The `gate_source()` /
`ungate_source()` API can be retained as a convenience for users who don't
need ongoing temporal alignment — internally mapped to a lightweight boolean
flag rather than the full watermark machinery.

### Why Option D First

1. **DAG-native propagation.** Gating a source automatically blocks all
   transitive descendants. The user doesn't need to know the DAG
   structure — they just declare which sources aren't ready yet.

2. **Minimal API surface.** Two functions plus one introspection view.
   No new parameters on `create_stream_table`.

3. **Zero ongoing overhead.** The gate is a boolean check at the start of
   each scheduler tick. Once all sources are ungated, the check is trivial.

4. **Clean composition path.** When watermarks land, the bootstrap gate
   becomes "has this source ever had a watermark advanced?" — no API
   breakage, just a richer underlying mechanism.

5. **Independent of watermark timeline.** Works equally well whether the
   external data has a meaningful timestamp dimension or not (e.g., a one-
   time CSV import has no temporal axis).

---

## 5. Detailed Design (Phase 1)

### 5.1 Catalog

Add a `gated` column to `pgtrickle.pgt_change_tracking`:

```sql
ALTER TABLE pgtrickle.pgt_change_tracking
  ADD COLUMN gated BOOLEAN NOT NULL DEFAULT false;
```

Alternatively, a standalone table:

```sql
CREATE TABLE pgtrickle.pgt_source_gates (
    source_relid  OID PRIMARY KEY,
    gated         BOOLEAN NOT NULL DEFAULT true,
    gated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    ungated_at    TIMESTAMPTZ,
    gated_by      TEXT  -- session_user / application_name
);
```

The standalone table is cleaner — it tracks gating history and doesn't
pollute the CDC tracking table with unrelated state.

### 5.2 SQL Functions

```sql
-- Gate a source table. All downstream STs are blocked from refreshing.
-- Idempotent: gating an already-gated source is a no-op.
pgtrickle.gate_source(source_table TEXT) → void

-- Ungate a source table. Downstream STs are unblocked if all their
-- transitive sources are ungated. Transactional: the ungate becomes
-- visible when the caller's transaction commits.
-- Idempotent: ungating an already-ungated source is a no-op.
pgtrickle.ungate_source(source_table TEXT) → void

-- Introspection: current gate state for all tracked sources.
pgtrickle.source_gates() → TABLE (
    source_table  TEXT,
    schema_name   TEXT,
    gated         BOOLEAN,
    gated_at      TIMESTAMPTZ,
    ungated_at    TIMESTAMPTZ,
    gated_by      TEXT
)
```

### 5.3 Scheduler Integration

In `run_scheduler_tick()`, before processing each ST in topological order:

```rust
fn is_source_gated(dag: &StDag, node_id: NodeId) -> bool {
    // Collect transitive source set for this ST
    let sources = dag.transitive_sources(node_id);
    // Check if any source has an active gate
    sources.iter().any(|src| {
        // Look up gate status from catalog or cached state
        source_gate_status(src.relid) == Gated
    })
}
```

If any transitive source is gated:
- Skip the ST for this tick.
- Log: `"pg_trickle: skipping {schema}.{name} — source {source} is gated (bootstrap)"`.
- Record `action = 'SKIP'` with `status = 'SKIPPED'` in `pgt_refresh_history`,
  with a descriptive skip reason.

### 5.4 Signal Mechanism

`ungate_source()` should signal the scheduler so the gating predicate is
re-evaluated promptly rather than waiting for the next natural tick:

- Option 1: `pg_notify('pgtrickle_gate_change', source_relid::text)` —
  lightweight, the scheduler already listens for notifications.
- Option 2: Bump the shared-memory DAG version counter — triggers a
  re-evaluation on the next tick. Heavier than needed since the DAG
  structure hasn't changed, only gating state.

**Recommendation:** Use `pg_notify` for a lightweight signal. The
scheduler can subscribe to a `pgtrickle_source_gate` channel and
re-evaluate gating predicates without a full DAG rebuild.

### 5.5 Caching

The scheduler should cache gate status at the start of each tick rather
than querying the catalog per-ST. A single query fetches all gated sources:

```sql
SELECT source_relid FROM pgtrickle.pgt_source_gates WHERE gated = true
```

This set is intersected with each ST's transitive source set during the
tick. The cache is invalidated on the next tick (or immediately if a
`pg_notify` signal is received).

### 5.6 Interaction with `initialize => false`

The `initialize` parameter on `create_stream_table` controls whether the
ST is populated at creation time. Bootstrap gating is orthogonal:

| `initialize` | Source gated | Behavior |
|--------------|-------------|----------|
| `true` | No | Normal: ST is created and immediately populated |
| `true` | Yes | ST creation succeeds, initial populate runs (with partial data). Subsequent refreshes are blocked until sources are ungated. **Recommendation:** warn at creation time that gated sources may produce incomplete initial results. |
| `false` | No | ST created empty. First scheduler tick triggers initial refresh. |
| `false` | Yes | ST created empty. Scheduler skips until all sources ungated. First refresh after ungating is the initial population. **This is the recommended bootstrap pattern.** |

**Recommended pattern for bootstrap:**

```sql
-- 1. Gate sources before ETL starts
SELECT pgtrickle.gate_source('orders');
SELECT pgtrickle.gate_source('order_lines');

-- 2. Create stream tables (initialize => false avoids premature population)
SELECT pgtrickle.create_stream_table('order_summary',
    'SELECT region, SUM(amount) FROM orders GROUP BY region',
    '1m', initialize => false);
SELECT pgtrickle.create_stream_table('order_report',
    'SELECT ... FROM order_summary JOIN line_summary ...',
    '1m', initialize => false);

-- 3. Run ETL processes (can be parallel, any order)
BEGIN;
  COPY orders FROM 's3://…';
  SELECT pgtrickle.ungate_source('orders');
COMMIT;

BEGIN;
  COPY order_lines FROM 's3://…';
  SELECT pgtrickle.ungate_source('order_lines');
COMMIT;

-- 4. Next scheduler tick: all sources ungated → STs initialize and refresh
```

---

## 6. Extended Use Cases

### A. Re-Bootstrap After Schema Change

An external system changes its schema, requiring a full re-import:

```sql
-- Gate the source, truncate, re-import
SELECT pgtrickle.gate_source('orders');
TRUNCATE orders;

-- Full re-import
COPY orders FROM 's3://warehouse/orders_v2.parquet';
SELECT pgtrickle.ungate_source('orders');
```

During the truncate + re-import window, downstream STs are not refreshed,
avoiding the churn of processing intermediate states during re-population.

### B. Coordinated Multi-Source Bootstrap

Multiple sources with different load times:

```sql
-- Gate all sources upfront
SELECT pgtrickle.gate_source('customers');
SELECT pgtrickle.gate_source('orders');
SELECT pgtrickle.gate_source('products');

-- ETL processes run independently, potentially in parallel
-- Each ungates its source upon completion
-- Downstream STs that depend on all three only start refreshing
-- when the last source is ungated
```

### C. Partial DAG Gating

Only gate sources that feed into a specific part of the DAG:

```
customers ──→ customer_stats ──┐
                               ├──→ customer_report
orders ──→ order_summary ──────┘
                │
                └──→ order_dashboard (only depends on orders)
```

If only `customers` is gated:
- `customer_stats` and `customer_report` are blocked (transitive dependency).
- `order_summary` and `order_dashboard` refresh normally (no dependency on
  gated source).

### D. Bootstrap with Existing Stream Tables

Adding a new source to an existing pipeline:

```sql
-- New source table, needs initial population
CREATE TABLE inventory (sku TEXT, qty INT);
SELECT pgtrickle.gate_source('inventory');

-- Create a new ST that joins existing and new sources
SELECT pgtrickle.create_stream_table('inventory_report',
    'SELECT o.*, i.qty FROM order_summary o JOIN inventory i ON ...',
    '1m', initialize => false);

-- ETL populates inventory
COPY inventory FROM 's3://…';
SELECT pgtrickle.ungate_source('inventory');
```

Existing STs (`order_summary`, etc.) continue refreshing normally since they
don't depend on the gated source.

---

## 7. Composability with Other Mechanisms

### Bootstrap Gating + Watermark Gating

Bootstrap gating and watermark gating serve different lifecycle phases:

```
┌─────────────────┐     ┌──────────────────────────────────┐
│  BOOTSTRAP PHASE │     │       STEADY-STATE PHASE          │
│                  │     │                                    │
│  gate_source()   │────▶│  advance_watermark(T) on each     │
│  ETL loads data  │     │  load cycle; watermark group       │
│  ungate_source() │     │  ensures temporal alignment        │
└─────────────────┘     └──────────────────────────────────┘
```

Both can be active simultaneously. The scheduler evaluates gates in order:

1. **Bootstrap gate:** Is any transitive source gated? → Skip.
2. **Watermark gate:** Are watermark groups aligned? → Skip if misaligned.
3. **Normal scheduling:** Check schedule, changes, etc. → Refresh.

When watermarks land, the bootstrap gate for a source can optionally be
auto-lifted when the first watermark is advanced (configurable):

```sql
SELECT pgtrickle.gate_source('orders',
    auto_ungate_on_watermark => true  -- lift gate on first advance_watermark()
);
```

### Bootstrap Gating + Diamond Consistency

Orthogonal. Diamond consistency governs how structurally related STs are
refreshed atomically. Bootstrap gating governs whether they refresh at all.
A diamond group where any source is gated simply doesn't refresh as a unit.

### Bootstrap Gating + `refresh_mode = 'IMMEDIATE'`

IMMEDIATE mode STs are refreshed synchronously within the source DML
transaction. Bootstrap gating doesn't apply to IMMEDIATE STs — they are
maintained on every write by definition. If the user doesn't want IMMEDIATE
STs to see partial bootstrap data, they should use DIFFERENTIAL mode during
bootstrap and switch to IMMEDIATE after ungating.

---

## 8. Open Questions

1. **Should `gate_source()` work on tables not yet tracked by pg_trickle?**
   If the user gates a table before creating any ST that depends on it,
   should pg_trickle record the gate in `pgt_source_gates` speculatively?
   This is useful for the "gate first, create STs second" pattern (§5.6).
   Proposed: yes — the gate table is independent of `pgt_change_tracking`.

2. **Should ungating be automatic after the first successful refresh?**
   Once all sources are ungated and the downstream ST completes its first
   full refresh, the gate has served its purpose. Should the gate rows be
   cleaned up automatically, or kept for audit? Proposed: keep the rows
   with `ungated_at` timestamp, but the scheduler ignores rows where
   `gated = false`.

3. **Should there be a timeout / stale gate warning?** If a source has
   been gated for longer than a configurable duration (e.g., 1 hour),
   pg_trickle could emit a warning: "source X has been gated for 1h —
   is the ETL still running?" This helps detect stuck ETL processes.

4. **Should `create_stream_table` auto-detect gated sources and set
   `initialize => false`?** If the user creates a ST whose sources include
   a gated source, should pg_trickle automatically skip initial population
   (since it would produce incomplete results)? Proposed: yes, with a
   `NOTICE` explaining why.

5. **Per-ST override?** Should a ST be able to opt out of bootstrap gating
   even if one of its sources is gated? Use case: a monitoring ST that
   intentionally shows partial progress during bootstrap. Proposed: add
   `bootstrap_gating => 'ignore'` on `create_stream_table` for this case.

6. **Bulk gate/ungate?** For pipelines with many sources, a convenience
   function:
   ```sql
   SELECT pgtrickle.gate_sources(ARRAY['orders', 'order_lines', 'products']);
   SELECT pgtrickle.ungate_sources(ARRAY['orders', 'order_lines', 'products']);
   ```

---

## 9. Implementation Steps

### Step 1 — Catalog: `pgt_source_gates`

Create table (§5.1), add to upgrade migration SQL. Structs in `catalog.rs`.

### Step 2 — `gate_source()` / `ungate_source()` SQL Functions

`#[pg_extern(schema = "pgtrickle")]` in `api.rs`. Validation (source must
be a valid relation), idempotency, transactional semantics, `pg_notify`
signal.

### Step 3 — Scheduler Gate Check

In `scheduler.rs`, at the start of each ST evaluation in topological order:
1. Load gated source set (cached per tick).
2. Compute transitive source set for the ST.
3. Intersect. If non-empty, skip with reason.

### Step 4 — `source_gates()` Introspection Function

Return current gate status for all tracked sources.

### Step 5 — `create_stream_table` Integration

When a new ST is created, check if any of its sources are gated. If so:
- Set `initialize => false` automatically (skip initial population).
- Emit `NOTICE`: "Source table X is currently gated; initial population
  deferred until all sources are ungated."

### Step 6 — Tests

| Test | Type | What it proves |
|------|------|----------------|
| `test_gate_source_blocks_downstream` | E2E | Gated source prevents all downstream ST refresh |
| `test_ungate_source_allows_refresh` | E2E | Ungating triggers refresh on next tick |
| `test_gate_multiple_sources_all_must_ungate` | E2E | ST with two gated sources waits for both |
| `test_gate_partial_dag` | E2E | Only STs depending on gated source are blocked |
| `test_gate_idempotent` | Unit | Double gate / double ungate are no-ops |
| `test_gate_transactional` | E2E | Ungate not visible until caller commits |
| `test_gate_create_st_auto_skip_init` | E2E | ST created with gated source skips initialization |
| `test_gate_with_immediate_mode` | E2E | IMMEDIATE STs unaffected by source gates |

### Step 7 — Documentation

- `docs/SQL_REFERENCE.md`: `gate_source()`, `ungate_source()`,
  `source_gates()`.
- `docs/tutorials/`: "Bootstrapping External Data Sources" tutorial.
- `CHANGELOG.md`.

---

## 10. Prior Art

1. **Apache Airflow Sensors** — Airflow's `ExternalTaskSensor` and
   `S3KeySensor` gate downstream tasks until upstream dependencies are
   satisfied. The bootstrap gate is analogous: a sensor that waits for
   "source data is fully loaded" before allowing downstream processing.

2. **dbt `ref()` Dependencies + `--defer`** — dbt's `--defer` flag allows
   models to reference artifacts from a prior run when upstream models
   haven't been rebuilt yet. The bootstrap gate is a stronger guarantee:
   don't run the model at all until inputs are ready, rather than using
   stale data.

3. **Kafka Consumer Group Readiness** — Kafka Streams applications wait
   until all partitions are assigned and initial offsets are committed
   before processing begins. The bootstrap gate applies the same principle
   to pg_trickle's scheduler.

4. **Flink Checkpoint Barriers** — Flink uses barriers in the data stream
   to coordinate exactly-once processing across operators. While more
   granular than bootstrap gating, the principle of "don't process until
   all inputs are at a known state" is the same.

5. **Materialize `SINCE` Frontier** — Materialize tracks a `SINCE`
   timestamp frontier per source. A source whose frontier is at the
   initial epoch hasn't produced any meaningful output yet. Downstream
   operators wait until all input frontiers advance past the epoch before
   producing results.

---

## 11. Relationship to Other Plans

| Plan | Relationship |
|------|--------------|
| [PLAN_WATERMARK_GATING.md](PLAN_WATERMARK_GATING.md) | Complementary. Bootstrap gating handles one-time initial-readiness. Watermark gating handles ongoing temporal alignment. Phase 2 subsumes bootstrap into watermarks via "NULL watermark = gated." |
| [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | Orthogonal. Diamond consistency governs structural refresh atomicity. Bootstrap gating governs whether refresh occurs at all. |
| [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md) | Orthogonal. Snapshot consistency ensures PG-level coherence. Bootstrap gating ensures external data completeness. |
| [PLAN_FUSE.md](PLAN_FUSE.md) | Complementary. Fuse halts refresh on anomalous change volume. Bootstrap gating halts refresh on incomplete source population. Both are pre-conditions evaluated before refresh. |
| [PLAN_TRANSACTIONAL_IVM.md](PLAN_TRANSACTIONAL_IVM.md) | Bootstrap gating does not apply to IMMEDIATE mode STs (maintained synchronously in the DML transaction). Users should defer IMMEDIATE mode activation until after bootstrap. |
