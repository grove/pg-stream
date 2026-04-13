# PLAN: Dog-Feeding — pg_trickle Monitoring Itself via Stream Tables

**Date:** 2026-04-13
**Status:** Proposed
**Scope:** Use pg_trickle stream tables over its own catalog and history tables
to power reactive adaptive logic, anomaly detection, and operational analytics.

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Architecture](#2-architecture)
3. [Bootstrap Problem & Safety Boundary](#3-bootstrap-problem--safety-boundary)
4. [Current Adaptive Logic Inventory](#4-current-adaptive-logic-inventory)
5. [Dog-Feeding Stream Tables](#5-dog-feeding-stream-tables)
6. [Reactive Policy Layer](#6-reactive-policy-layer)
7. [Implementation Phases](#7-implementation-phases)
8. [Testing Strategy](#8-testing-strategy)
9. [Risks & Mitigations](#9-risks--mitigations)
10. [Future Extensions](#10-future-extensions)

---

## 1. Motivation

pg_trickle already maintains rich operational data in its own catalog:

| Table | Rows/cycle | Content |
|-------|-----------|---------|
| `pgt_refresh_history` | 1 per refresh | Duration, action, rows affected, status, errors |
| `pgt_stream_tables` | 1 per ST | Threshold, effective mode, consecutive errors, fuse state |
| `pgt_dependencies` | N per ST | Source types, CDC modes, schema fingerprints |

Today this data is consumed in two limited ways:

1. **In-process Rust** — `compute_adaptive_threshold()` compares the current
   INCR time against the last FULL time. One cycle of memory. No cross-ST
   awareness, no trend detection, no rolling window.
2. **On-demand SQL** — `refresh_efficiency()`, `st_refresh_stats()`,
   `check_cdc_health()` compute aggregates at query time by scanning
   `pgt_refresh_history` on every call.

Neither approach delivers **reactive, continuously-maintained analytics**. The
diagnostic functions scan the full history table on every call. The adaptive
threshold sees one cycle at a time and converges via a conservative one-step
formula (0.80×/0.90×/1.10× multiplier clamped to [0.01, 0.80]).

pg_trickle is designed to maintain exactly this kind of analytics — incremental
aggregates over append-only sources with low-latency refresh. Dog-feeding
validates the extension on a non-trivial workload, replaces repeated full scans
with maintained views, and enables a qualitatively new **reactive policy layer**
that learns from multi-cycle, cross-ST patterns.

### Goals

- Replace full-scan diagnostic functions with maintained stream tables
- Enable multi-cycle trend detection for threshold tuning
- Detect cross-ST scheduling interference patterns
- Surface anomalies (duration spikes, error bursts, spill trends) reactively
  rather than on manual inspection
- Validate pg_trickle on its own operational workload

### Non-Goals

- Replacing the Rust hot path — `execute_differential_refresh` and the
  scheduler dispatch loop remain in Rust (§3)
- Automatic unsupervised reconfiguration — the policy layer is advisory by
  default; auto-apply is opt-in per GUC
- Supporting dog-feeding on day one of a fresh install — stream tables require
  history to exist first

---

## 2. Architecture

The architecture separates the **control plane** (Rust, in-transaction,
latency-critical) from the **analytics plane** (stream tables, async, off
critical path) and the **policy layer** (advisory reads + optional auto-apply).

```
┌─────────────────────────────────────────────────────────────────────┐
│  CONTROL PLANE  (Rust, hot path, in-transaction)                    │
│                                                                     │
│  scheduler.rs     → execute_scheduled_refresh()                     │
│  refresh.rs       → compute_adaptive_threshold()                    │
│  catalog.rs       → RefreshRecord::insert() / complete()            │
│                                                                     │
│  WRITES TO:                                                         │
│    pgtrickle.pgt_refresh_history     (append-only, 1 row/refresh)   │
│    pgtrickle.pgt_stream_tables       (update auto_threshold, etc.)  │
│    pgtrickle_changes.changes_<oid>   (CDC buffers)                  │
└────────────────────┬────────────────────────────────────────────────┘
                     │ CDC triggers on pgt_refresh_history
                     │ (append-only → INSERT triggers only)
┌────────────────────▼────────────────────────────────────────────────┐
│  ANALYTICS PLANE  (pg_trickle stream tables, async refresh)         │
│                                                                     │
│  DF-1: pgtrickle.df_efficiency_rolling                              │
│  DF-2: pgtrickle.df_anomaly_signals                                 │
│  DF-3: pgtrickle.df_threshold_advice                                │
│  DF-4: pgtrickle.df_cdc_buffer_trends                               │
│  DF-5: pgtrickle.df_scheduling_interference                         │
│                                                                     │
│  Refreshed by the normal scheduler (schedule => '48s' or slower)    │
│  Uses DIFFERENTIAL mode — its own source is append-only             │
└────────────────────┬────────────────────────────────────────────────┘
                     │ Advisory reads (separate sessions/workers)
┌────────────────────▼────────────────────────────────────────────────┐
│  POLICY LAYER  (optional, off critical path)                        │
│                                                                     │
│  - Threshold advisory:  SELECT * FROM pgtrickle.df_threshold_advice │
│  - Auto-apply worker:   reads df_threshold_advice, issues           │
│    ALTER STREAM TABLE ... SET auto_threshold = <recommended>        │
│  - Anomaly alerting:    LISTEN pg_trickle_alert on anomaly rows     │
│  - Dashboard export:    Grafana reads df_efficiency_rolling          │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Invariant

The analytics plane is **strictly downstream** of the control plane. No control
plane decision depends on the analytics plane being populated or refreshed.
If all `df_*` stream tables are dropped, the extension operates identically
to today. This eliminates the bootstrapping cycle described in §3.

---

## 3. Bootstrap Problem & Safety Boundary

### The Problem

If the scheduler needed a dog-feeding stream table to decide whether to refresh
a stream table, it would need pg_trickle to be running to run pg_trickle. This
creates a circular dependency:

```
scheduler tick
  → needs df_threshold_advice to pick FULL vs DIFF
  → df_threshold_advice is a stream table
  → needs a scheduler tick to refresh
  → deadlock
```

### The Solution: Strict Layering

| Component | Layer | Depends on dog-feeding? |
|-----------|-------|------------------------|
| `compute_adaptive_threshold()` | Control plane | **No** — uses only `auto_threshold` and `last_full_ms` from catalog |
| `compute_adaptive_poll_ms()` | Control plane | **No** — uses only in-memory state |
| `execute_scheduled_refresh()` | Control plane | **No** — reads `pgt_stream_tables` directly |
| `df_efficiency_rolling` | Analytics plane | **No** — reads `pgt_refresh_history` |
| `df_threshold_advice` | Analytics plane | **No** — reads `pgt_refresh_history` + `pgt_stream_tables` |
| Policy auto-apply worker | Policy layer | **Yes** — reads analytics plane stream tables |

The policy layer is the **only** component that reads dog-feeding stream tables,
and it runs in a separate session, off the critical path, behind an opt-in GUC.

### Self-Referential Refresh

The dog-feeding stream tables themselves are refreshed by the same scheduler
that refreshes user stream tables. They appear in the DAG as normal ST nodes.
Their refresh generates `pgt_refresh_history` rows, which are then consumed by
the next refresh of the analytics stream tables — a healthy feedback loop,
not a deadlock, because:

1. Each refresh reads history **up to the current tick watermark** (CSS1)
2. The dog-feeding STs have a relaxed schedule (`'48s'` or longer)
3. They are CDC'd via INSERT triggers on `pgt_refresh_history` (append-only)

### Startup Sequence

On extension install or upgrade:

1. Control plane starts normally with the existing Rust-only adaptive logic
2. After the first few refreshes populate `pgt_refresh_history`, the analytics
   stream tables can be created (manually or via `pgtrickle.setup_dog_feeding()`)
3. The policy layer activates only after the analytics STs are populated

If a dog-feeding ST enters SUSPENDED state, it is treated like any other
suspended ST — the control plane continues operating on its own.

---

## 4. Current Adaptive Logic Inventory

### 4.1 Threshold Tuning

**Function:** `compute_adaptive_threshold(current, incr_ms, full_ms) → f64`
**Location:** `src/refresh.rs:5903–5921`

| Ratio (INCR/FULL) | Action | Multiplier |
|--------------------|--------|------------|
| ≥ 0.90 | INCR ≈ FULL, lower aggressively | 0.80× |
| ≥ 0.70 | INCR getting expensive | 0.90× |
| ≤ 0.30 | INCR much faster, raise | 1.10× (cap 0.80) |
| 0.30–0.70 | Stable | 1.00× |

**Limitation:** Single-cycle comparison. If INCR is 0.85× of FULL for 10
cycles and then 0.25× for 1 cycle, the threshold oscillates. No trend memory.

### 4.2 Adaptive Poll

**Function:** `compute_adaptive_poll_ms(current, had_completion, has_inflight, base) → u64`
**Location:** `src/scheduler.rs:1338–1355`

Exponential backoff: 20ms → 40 → 80 → 160 → 200ms max. Resets to 20ms on
any worker completion.

**Limitation:** No workload-awareness. A burst of 50 STs due at the same tick
gets the same 20ms poll as a single ST completing.

### 4.3 Spill Detection

**Location:** `src/monitor.rs:425`

Increments a consecutive counter when `temp_blks_written` exceeds the GUC
threshold. After `spill_consecutive_limit` (default 3) consecutive spills,
forces FULL refresh.

**Limitation:** Binary (spill/no-spill). No trend analysis — a slowly growing
spill that stays just below the threshold is invisible.

### 4.4 Auto-Suspend

**Location:** `src/scheduler.rs:5040`

Suspends ST after `max_consecutive_errors` (default 3) failures. Requires
manual `resume_stream_table()`.

**Limitation:** No pattern analysis — cannot distinguish "always fails on
Mondays at 3am" from "permanently broken."

### 4.5 Falling-Behind Detection

**Location:** `src/scheduler.rs:5037`

Emits EC-11 alert when refresh duration exceeds 80% of schedule interval.

**Limitation:** Point-in-time check, no rolling degradation detection.

---

## 5. Dog-Feeding Stream Tables

All dog-feeding STs use the `pgtrickle` schema and the `df_` prefix
(dog-feeding). Each targets a specific gap identified in §4.

### DF-1: Rolling Efficiency Statistics

**Purpose:** Replace `refresh_efficiency()` full scans with a maintained view.
Provide rolling-window aggregates for threshold tuning and dashboard display.

```sql
CREATE STREAM TABLE pgtrickle.df_efficiency_rolling
WITH (schedule => '48s', refresh_mode => 'DIFFERENTIAL')
AS
SELECT
    h.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    count(*)                                                     AS total_refreshes,
    count(*) FILTER (WHERE h.action = 'DIFFERENTIAL')            AS diff_count,
    count(*) FILTER (WHERE h.action = 'FULL')                    AS full_count,
    avg(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)
        FILTER (WHERE h.action = 'DIFFERENTIAL')                 AS avg_diff_ms,
    avg(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)
        FILTER (WHERE h.action = 'FULL')                         AS avg_full_ms,
    percentile_cont(0.95) WITHIN GROUP (
        ORDER BY EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000
    ) FILTER (WHERE h.action = 'DIFFERENTIAL')                   AS p95_diff_ms,
    avg(h.delta_row_count::float
        / NULLIF(h.rows_inserted + h.rows_deleted, 0))          AS avg_change_ratio,
    max(h.start_time)                                            AS last_refresh_at
FROM pgtrickle.pgt_refresh_history h
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
WHERE h.status = 'COMPLETED'
  AND h.start_time > now() - interval '1 hour'
GROUP BY h.pgt_id, st.pgt_schema, st.pgt_name;
```

**CDC source:** `pgt_refresh_history` (append-only, INSERT trigger)

**Replaces:** `pgtrickle.refresh_efficiency()` on-demand function

**Note on percentile_cont:** This uses an ordered-set aggregate. If not
supported in DIFFERENTIAL mode, the view can fall back to FULL refresh with
the `percentile_cont` column computed, or omit it and compute it in a
downstream DF ST. The core avg/count aggregates are fully differentiable.

### DF-2: Anomaly Signals

**Purpose:** Detect duration spikes, error bursts, and mode oscillation by
comparing recent behavior against rolling baselines from DF-1.

```sql
CREATE STREAM TABLE pgtrickle.df_anomaly_signals
WITH (schedule => '48s', refresh_mode => 'DIFFERENTIAL')
AS
SELECT
    h.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    -- Duration spike: last refresh > 3× rolling average
    CASE WHEN latest.duration_ms > 3.0 * NULLIF(eff.avg_diff_ms, 0)
         THEN 'DURATION_SPIKE'
    END AS duration_anomaly,
    latest.duration_ms AS last_duration_ms,
    eff.avg_diff_ms AS baseline_diff_ms,
    -- Error burst: 2+ failures in last 10 refreshes
    count(*) FILTER (WHERE h.status = 'FAILED'
                       AND h.start_time > now() - interval '10 minutes')
        AS recent_failures,
    -- Mode oscillation: alternating FULL/DIFF in last 5 cycles
    count(DISTINCT h.action) FILTER (
        WHERE h.start_time > now() - interval '5 minutes'
          AND h.action IN ('FULL', 'DIFFERENTIAL')
    ) AS distinct_modes_recent,
    st.auto_threshold,
    st.effective_refresh_mode
FROM pgtrickle.pgt_refresh_history h
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = h.pgt_id
LEFT JOIN pgtrickle.df_efficiency_rolling eff ON eff.pgt_id = h.pgt_id
LEFT JOIN LATERAL (
    SELECT EXTRACT(EPOCH FROM (h2.end_time - h2.start_time)) * 1000 AS duration_ms
    FROM pgtrickle.pgt_refresh_history h2
    WHERE h2.pgt_id = h.pgt_id AND h2.status = 'COMPLETED'
    ORDER BY h2.start_time DESC LIMIT 1
) latest ON true
WHERE h.start_time > now() - interval '10 minutes'
GROUP BY h.pgt_id, st.pgt_schema, st.pgt_name,
         latest.duration_ms, eff.avg_diff_ms,
         st.auto_threshold, st.effective_refresh_mode;
```

**Depends on:** DF-1 (via join to `df_efficiency_rolling`)

**DAG position:** Downstream of DF-1 → refreshed after DF-1 completes

### DF-3: Threshold Advice

**Purpose:** Multi-cycle threshold recommendation that replaces the single-step
`compute_adaptive_threshold()` convergence with a window-based analysis.

```sql
CREATE STREAM TABLE pgtrickle.df_threshold_advice
WITH (schedule => '96s', refresh_mode => 'DIFFERENTIAL')
AS
SELECT
    eff.pgt_id,
    eff.pgt_schema,
    eff.pgt_name,
    st.auto_threshold AS current_threshold,
    -- Recommended threshold based on rolling DIFF/FULL ratio
    CASE
        WHEN eff.avg_diff_ms IS NOT NULL AND eff.avg_full_ms IS NOT NULL
             AND eff.avg_full_ms > 0
        THEN LEAST(0.80, GREATEST(0.01,
            -- If DIFF is consistently faster, allow higher threshold
            CASE WHEN eff.avg_diff_ms / eff.avg_full_ms <= 0.30 THEN
                LEAST(st.auto_threshold * 1.20, 0.80)
            -- If DIFF is approaching FULL cost, lower threshold
            WHEN eff.avg_diff_ms / eff.avg_full_ms >= 0.80 THEN
                GREATEST(st.auto_threshold * 0.70, 0.01)
            -- Moderate range: nudge toward optimal
            ELSE st.auto_threshold
            END
        ))
        ELSE st.auto_threshold  -- No data yet, keep current
    END AS recommended_threshold,
    -- Confidence based on sample size
    CASE
        WHEN eff.total_refreshes >= 20 AND eff.diff_count >= 5
             AND eff.full_count >= 2 THEN 'HIGH'
        WHEN eff.total_refreshes >= 10 THEN 'MEDIUM'
        ELSE 'LOW'
    END AS confidence,
    -- Recommendation reason
    CASE
        WHEN eff.avg_diff_ms IS NOT NULL AND eff.avg_full_ms IS NOT NULL
             AND eff.avg_full_ms > 0
        THEN CASE
            WHEN eff.avg_diff_ms / eff.avg_full_ms <= 0.30
                THEN 'DIFF is ' || round((1.0 - eff.avg_diff_ms / eff.avg_full_ms)::numeric * 100)
                     || '% faster — raise threshold to allow more DIFF'
            WHEN eff.avg_diff_ms / eff.avg_full_ms >= 0.80
                THEN 'DIFF is only ' || round((1.0 - eff.avg_diff_ms / eff.avg_full_ms)::numeric * 100)
                     || '% faster — lower threshold to prefer FULL sooner'
            ELSE 'Current threshold is well-calibrated'
        END
        ELSE 'Insufficient data — need both DIFF and FULL observations'
    END AS reason,
    eff.avg_diff_ms,
    eff.avg_full_ms,
    eff.diff_count,
    eff.full_count,
    eff.avg_change_ratio
FROM pgtrickle.df_efficiency_rolling eff
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = eff.pgt_id;
```

**Depends on:** DF-1 (via join to `df_efficiency_rolling`)

**Schedule:** Slower (96s) — threshold changes should be deliberate, not frantic

### DF-4: CDC Buffer Trends

**Purpose:** Track change buffer growth rates to predict spills before they
happen and detect sources with unexpectedly high write volume.

```sql
CREATE STREAM TABLE pgtrickle.df_cdc_buffer_trends
WITH (schedule => '48s', refresh_mode => 'DIFFERENTIAL')
AS
SELECT
    d.pgt_id,
    st.pgt_schema,
    st.pgt_name,
    d.source_relid,
    d.source_relid::regclass::text AS source_table,
    d.cdc_mode,
    -- Buffer size snapshot (current pending rows)
    (SELECT count(*)
     FROM pgtrickle_changes.changes_source
     WHERE source = d.source_relid) AS pending_rows,
    -- Rows consumed per refresh in the last hour
    avg(h.delta_row_count) AS avg_delta_per_refresh,
    max(h.delta_row_count) AS max_delta_per_refresh,
    -- Refresh frequency in the last hour
    count(h.*) AS refreshes_last_hour
FROM pgtrickle.pgt_dependencies d
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = d.pgt_id
LEFT JOIN pgtrickle.pgt_refresh_history h
    ON h.pgt_id = d.pgt_id
   AND h.status = 'COMPLETED'
   AND h.start_time > now() - interval '1 hour'
WHERE d.source_type = 'TABLE'
GROUP BY d.pgt_id, st.pgt_schema, st.pgt_name,
         d.source_relid, d.cdc_mode;
```

**Note:** The inner `count(*)` from `pgtrickle_changes.changes_source` is a
dynamic subquery. Depending on the DVM parser's support for correlated
subqueries over partitioned/dynamic table names, this may need to be
restructured as a helper function or replaced with a periodic snapshot table.
This is flagged as a design risk in §9.

### DF-5: Scheduling Interference

**Purpose:** Detect when concurrent refreshes of different STs interfere with
each other (resource contention causing duration spikes in overlapping windows).

```sql
CREATE STREAM TABLE pgtrickle.df_scheduling_interference
WITH (schedule => '96s', refresh_mode => 'DIFFERENTIAL')
AS
SELECT
    a.pgt_id AS pgt_id_a,
    b.pgt_id AS pgt_id_b,
    sta.pgt_name AS st_name_a,
    stb.pgt_name AS st_name_b,
    count(*) AS overlap_count,
    avg(EXTRACT(EPOCH FROM (a.end_time - a.start_time)) * 1000) AS avg_duration_a_ms,
    avg(EXTRACT(EPOCH FROM (b.end_time - b.start_time)) * 1000) AS avg_duration_b_ms
FROM pgtrickle.pgt_refresh_history a
JOIN pgtrickle.pgt_refresh_history b
    ON a.pgt_id < b.pgt_id  -- avoid self-join and duplicates
   AND a.start_time < b.end_time
   AND b.start_time < a.end_time  -- overlap condition
JOIN pgtrickle.pgt_stream_tables sta ON sta.pgt_id = a.pgt_id
JOIN pgtrickle.pgt_stream_tables stb ON stb.pgt_id = b.pgt_id
WHERE a.status = 'COMPLETED'
  AND b.status = 'COMPLETED'
  AND a.start_time > now() - interval '1 hour'
GROUP BY a.pgt_id, b.pgt_id, sta.pgt_name, stb.pgt_name
HAVING count(*) >= 3;  -- Only report frequent overlaps
```

**Use case:** If STs A and B always overlap and A's duration doubles when they
do, the parallel DAG dispatcher could learn to sequence them. This is the input
for a future "anti-affinity" scheduling policy.

**Refresh mode note:** The self-join on `pgt_refresh_history` with an overlap
condition is complex for DIFFERENTIAL mode. This ST may need to run as FULL
refresh initially. If the history window is bounded (1 hour), FULL refresh
over the bounded window is efficient.

---

## 6. Reactive Policy Layer

The policy layer is **optional** and **off by default**. It reads the
dog-feeding stream tables and translates analytics into configuration changes.

### 6.1 Advisory Mode (Default)

Stream tables are queryable by operators and dashboards. No automated changes.

```sql
-- "Which of my STs have a poorly calibrated threshold?"
SELECT * FROM pgtrickle.df_threshold_advice
WHERE confidence IN ('HIGH', 'MEDIUM')
  AND abs(recommended_threshold - current_threshold) > 0.05;

-- "Are any STs experiencing anomalies right now?"
SELECT * FROM pgtrickle.df_anomaly_signals
WHERE duration_anomaly IS NOT NULL
   OR recent_failures >= 2;
```

### 6.2 Auto-Apply Mode (Opt-In)

Controlled by a new GUC:

```
pg_trickle.dog_feeding_auto_apply = off  (default)
                                    threshold_only
                                    full
```

When `threshold_only`:
- A background task reads `df_threshold_advice` after each refresh cycle
- If confidence is HIGH and the recommended threshold differs from current by
  > 5%, applies `ALTER STREAM TABLE ... SET auto_threshold = <recommended>`
- Changes are logged to `pgt_refresh_history` with `initiated_by = 'DOG_FEED'`
- Rate-limited: at most one threshold change per ST per 10 minutes

When `full`:
- Also applies scheduling hints from `df_scheduling_interference`
- Can adjust `refresh_tier` for chronically slow STs
- Can pre-emptively lower `max_delta_fraction` for spill-trending STs

### 6.3 NOTIFY Integration

Anomaly signals can trigger NOTIFY events on the existing `pg_trickle_alert`
channel:

```json
{
  "event": "dog_feed_anomaly",
  "schema": "public",
  "name": "orders_summary",
  "anomaly": "DURATION_SPIKE",
  "last_ms": 4500,
  "baseline_ms": 1200,
  "recommendation": "Lower auto_threshold from 0.15 to 0.10"
}
```

---

## 7. Implementation Phases

### Phase 1: Foundation (Low Risk)

**Goal:** Prove that `pgt_refresh_history` works as a CDC source and that
stream tables over it refresh correctly.

1. Verify `pgt_refresh_history` gets CDC triggers during `create_stream_table`
   (it lives in the `pgtrickle` schema, which currently may be excluded from
   CDC — check and fix if needed)
2. Create DF-1 (`df_efficiency_rolling`) manually via `create_stream_table()`
3. Verify DIFFERENTIAL refresh works (append-only source → INSERT-only deltas)
4. Compare output against `refresh_efficiency()` for correctness
5. Benchmark: maintained DF-1 read vs. `refresh_efficiency()` full scan

**Deliverables:**
- E2E test: create DF-1 over `pgt_refresh_history`, insert test history rows,
  refresh, verify aggregates
- Documentation: "Dog-feeding quick start" in SQL_REFERENCE.md

### Phase 2: Anomaly Detection (Medium Risk)

**Goal:** Implement DF-2 and DF-3 as a dependent DAG downstream of DF-1.

1. Create DF-2 (`df_anomaly_signals`) dependent on DF-1
2. Create DF-3 (`df_threshold_advice`) dependent on DF-1
3. Verify DAG ordering: DF-1 refreshes first, then DF-2 and DF-3
4. Verify that the dog-feeding STs' own refresh history rows don't cause
   circular confusion (they should appear in DF-1 naturally and harmlessly)

**Deliverables:**
- E2E test: inject synthetic history (fast DIFF, slow FULL), verify
  `df_threshold_advice` recommends raising the threshold
- E2E test: inject a duration spike, verify `df_anomaly_signals` detects it

### Phase 3: CDC Buffer & Interference (Medium Risk)

**Goal:** Implement DF-4 and DF-5 for operational visibility.

1. Create DF-4 (`df_cdc_buffer_trends`) — may need helper function for
   dynamic change buffer table names
2. Create DF-5 (`df_scheduling_interference`) — likely FULL refresh initially
3. Test under parallel refresh with overlapping STs

**Deliverables:**
- E2E test: create 3 STs with overlapping schedules, verify DF-5 detects
  overlap patterns
- Design decision: DF-4 dynamic table name strategy (helper fn vs. snapshot)

### Phase 4: Setup Helper & GUC (Low Risk)

**Goal:** Make dog-feeding easy to enable.

1. Implement `pgtrickle.setup_dog_feeding()` — creates all DF STs in one call
2. Implement `pgtrickle.teardown_dog_feeding()` — drops all DF STs cleanly
3. Add `pg_trickle.dog_feeding_auto_apply` GUC (off / threshold_only / full)
4. Document in CONFIGURATION.md and SQL_REFERENCE.md

**Deliverables:**
- `setup_dog_feeding()` / `teardown_dog_feeding()` SQL functions
- GUC registered in `src/config.rs`

### Phase 5: Auto-Apply Worker (Higher Risk)

**Goal:** Close the loop — analytics inform configuration changes.

1. Implement the auto-apply logic in a scheduler post-tick hook
2. Read `df_threshold_advice` after each coordinator tick
3. Apply threshold changes with rate limiting and logging
4. Add `initiated_by = 'DOG_FEED'` to `pgt_refresh_history` for audit trail

**Deliverables:**
- Auto-apply worker with rate limiting
- E2E test: enable `threshold_only`, inject history making DIFF consistently
  faster, verify threshold increases automatically
- E2E test: verify rate limiting (no more than 1 change per 10 min per ST)

### Phase 6: NOTIFY & Dashboard Integration (Low Risk)

**Goal:** Surface anomalies to external systems.

1. Emit `dog_feed_anomaly` NOTIFY events when DF-2 detects anomalies
2. Add Grafana dashboard JSON for DF-1 and DF-2 visualization
3. Document LISTEN workflow for anomaly alerting

---

## 8. Testing Strategy

### Unit Tests

- `compute_adaptive_threshold()` vs. `df_threshold_advice` recommendation
  parity: given the same inputs, both approaches should agree on direction
  (raise/lower/keep) even if magnitude differs

### E2E Tests

| Test | Phase | Validates |
|------|-------|-----------|
| `test_df_efficiency_rolling_matches_refresh_efficiency` | 1 | DF-1 output ≈ `refresh_efficiency()` |
| `test_df_history_as_cdc_source` | 1 | Triggers fire on `pgt_refresh_history` |
| `test_df_dag_ordering` | 2 | DF-1 refreshes before DF-2, DF-3 |
| `test_df_self_referential_harmless` | 2 | DF STs' own history rows don't cause issues |
| `test_df_threshold_spike_detection` | 2 | DF-3 detects threshold miscalibration |
| `test_df_anomaly_duration_spike` | 2 | DF-2 detects 3× duration spike |
| `test_df_scheduling_overlap` | 3 | DF-5 detects concurrent refresh overlap |
| `test_df_setup_teardown` | 4 | Helper functions create/drop cleanly |
| `test_df_auto_apply_threshold` | 5 | Auto-apply adjusts threshold correctly |
| `test_df_auto_apply_rate_limit` | 5 | No more than 1 change per 10 min |
| `test_df_survives_own_suspension` | 5 | Control plane unaffected if DF ST suspends |

### Property Tests

- Inject random refresh history sequences (varying durations, modes, errors)
- Verify DF-3 recommended threshold is always within [0.01, 0.80]
- Verify DF-2 anomaly detection has no false positives on steady-state workloads

### Stability

- Soak test: run dog-feeding under 100 user STs for 1 hour, verify no memory
  growth, no cascading failures, no scheduler stalls

---

## 9. Risks & Mitigations

### R1: Recursive CDC Amplification

**Risk:** Dog-feeding STs refresh → generate history rows → trigger CDC →
schedule another dog-feeding refresh → generate more history rows → ...

**Mitigation:** Each refresh generates exactly 1 history row. A dog-feeding
cycle of 5 STs generates 5 rows per tick. At 48s schedule, that's 375
rows/hour — negligible compared to user workload. The system is convergent:
each dog-feeding refresh reads all accumulated rows and produces a fixed-size
aggregate output, not an amplifying chain.

### R2: Bootstrapping on Empty History

**Risk:** `setup_dog_feeding()` called on a fresh install with 0 history rows.
Stream tables would be empty and useless.

**Mitigation:** `setup_dog_feeding()` checks `pgt_refresh_history` row count.
If < 50 rows, emits a WARNING and proceeds — the STs will populate naturally
as history accumulates. Documentation notes the warm-up period.

### R3: Dynamic Table Names in DF-4

**Risk:** CDC buffer tables are named `pgtrickle_changes.changes_<oid>` and
the set of tables changes as STs are created/dropped. A static SQL query
cannot reference them.

**Mitigation:** Use a helper function `pgtrickle.cdc_buffer_row_counts()`
that dynamically queries each change buffer table and returns a set. DF-4
joins against this function rather than individual tables. If function-call
sources aren't DIFFERENTIAL-eligible, DF-4 runs as FULL (bounded window,
so this is efficient).

### R4: Ordered-Set Aggregates (percentile_cont)

**Risk:** `percentile_cont` in DF-1 is an ordered-set aggregate that may
not be supported in DIFFERENTIAL mode.

**Mitigation:** Two options:
1. Use approximation (`percentile_cont` over bounded window + FULL refresh)
2. Remove `p95_diff_ms` from DF-1 and compute it in a separate FULL-mode ST
   that DF-2 joins against

Phase 1 validates which approach works.

### R5: Schema Coupling

**Risk:** Upgrade migrations that change `pgt_refresh_history` columns
would break dog-feeding ST definitions.

**Mitigation:** Dog-feeding STs reference only stable columns (`pgt_id`,
`action`, `status`, `start_time`, `end_time`, `rows_inserted`, `rows_deleted`,
`delta_row_count`). These columns have been stable since v0.5.0. If a column
is renamed, `teardown_dog_feeding()` + `setup_dog_feeding()` re-creates the
STs with updated definitions. The upgrade migration script can automate this.

### R6: Performance Overhead

**Risk:** 5 additional stream tables increase scheduler work per tick.

**Mitigation:** Dog-feeding STs use relaxed schedules (48s–96s). At these
intervals, the overhead is 5 extra DIFFERENTIAL refreshes every 48s over
append-only sources — microseconds of incremental work. Under load testing,
if overhead exceeds 1% of total scheduler CPU, the schedules can be relaxed
further or the STs moved to `refresh_tier = 'warm'`.

---

## 10. Future Extensions

### 10.1 Cross-Database Dog-Feeding

When multi-database support (G17-MDB) ships, dog-feeding STs could aggregate
refresh history across all databases on the cluster, detecting patterns that
span database boundaries (e.g., shared I/O contention).

### 10.2 Workload-Aware Poll Intervals

Replace `compute_adaptive_poll_ms()` exponential backoff with a signal from
DF-5 (`df_scheduling_interference`): if the current tick's ready set is known
to contain contending STs, increase the dispatch interval pre-emptively rather
than discovering contention after the fact.

### 10.3 Predictive Spill Prevention

DF-4 (`df_cdc_buffer_trends`) growing at a rate that will exceed
`spill_threshold_blocks` within 2 refresh cycles → pre-emptively trigger
an early FULL refresh before the spill occurs.

### 10.4 SLA-Aware Threshold Tuning

Combine `freshness_deadline` from history records with DF-1 duration stats
to recommend thresholds that satisfy freshness SLAs rather than purely
optimizing cost. A ST that must be fresh within 5s tolerance should have a
lower threshold (prefer FULL for predictability) than one with 60s tolerance.

### 10.5 Grafana Dog-Feeding Dashboard

Pre-built Grafana dashboard that reads DF-1 through DF-5, providing out-of-box
operational visibility:
- Refresh throughput timeline (from DF-1)
- Anomaly heatmap (from DF-2)
- Threshold calibration scatter plot (from DF-3)
- CDC buffer growth sparklines (from DF-4)
- Interference matrix (from DF-5)

### 10.6 Integration with recommend_refresh_mode()

The planned `recommend_refresh_mode()` diagnostic function
(PLAN_DIAGNOSTICS_FUNCTION.md) can read directly from `df_threshold_advice`
instead of computing on demand, combining the best of both worlds: maintained
analytics for speed, on-demand diagnostics for depth.
