# PLAN_TIMESCALEDB.md — TimescaleDB Synergy & Lessons for pg_trickle

Date: 2026-03-01
Status: RESEARCH

---

## 1. Executive Summary

TimescaleDB and pg_trickle are **complementary** PostgreSQL extensions solving
different problems — partitioned time-series storage vs incremental view
maintenance. They don't conflict at the storage layer and can coexist.

However, there is a concrete **synergy opportunity**: pg_trickle can provide
rich IVM over hypertables where TimescaleDB's Continuous Aggregates are limited
(no complex JOINs, no subqueries, no window functions, no cross-table
aggregation). Several TimescaleDB design patterns are also worth studying for
adoption in pg_trickle.

This document catalogues: (1) where the projects complement each other,
(2) specific synergy scenarios, (3) engineering challenges for combined use,
(4) design ideas worth borrowing, and (5) a phased action plan.

---

## 2. Comparison

| | TimescaleDB | pg_trickle |
|---|---|---|
| Core problem | Efficient time-series storage & querying | Keeping materialized views fresh incrementally |
| Key mechanism | Hypertables — auto-partitioning by time, compression, retention policies | Differential dataflow (DBSP) — delta query derivation from operator trees |
| IVM approach | Continuous Aggregates — restricted to `time_bucket` GROUP BY on a single hypertable | Stream Tables — broad SQL (JOINs, CTEs, window functions, subqueries, set operations) over any table |
| Change tracking | Invalidation log per hypertable chunk/time-bucket | Row-level triggers → WAL logical replication |
| Workload model | Append-heavy time-series (INSERT-dominant) | General OLTP (INSERT/UPDATE/DELETE) |
| Refresh model | Materialization policies on invalidated buckets | Scheduled or on-demand delta refresh over change buffers |
| Pipeline support | Cagg-on-cagg (hierarchical rollups) | Stream-table-on-stream-table DAG with demand-driven scheduling |
| Storage | Compressed columnar chunks for old data | Standard PostgreSQL heap tables |
| License | Apache-2.0 (community) / Timescale License (enterprise features) | Apache-2.0 |

---

## 3. Synergy Scenarios

### 3.1. Rich SQL over Hypertables

Continuous Aggregates are limited to `GROUP BY time_bucket(...)` on a single
hypertable with scalar aggregates. Users cannot:

- JOIN a hypertable with a dimension table (e.g., `sensor_readings JOIN devices`)
- Use window functions (`ROW_NUMBER`, `LAG`, `LEAD`)
- Use subqueries or CTEs in the aggregate definition
- Combine data from multiple hypertables
- Use `DISTINCT ON`, `UNION`, `INTERSECT`, `EXCEPT`

pg_trickle stream tables support all of these. A user could define:

```sql
SELECT pgtrickle.create_stream_table(
    'enriched_hourly_readings',
    $$
    SELECT
        time_bucket('1 hour', r.ts) AS bucket,
        d.facility,
        d.region,
        avg(r.temperature) AS avg_temp,
        max(r.temperature) - min(r.temperature) AS temp_spread,
        count(*) AS reading_count
    FROM sensor_readings r
    JOIN devices d ON d.device_id = r.device_id
    WHERE d.is_active
    GROUP BY 1, 2, 3
    $$,
    schedule := '5 minutes'
);
```

This is impossible with a Continuous Aggregate alone.

### 3.2. Cross-Hypertable Aggregation

Organizations often have multiple hypertables (e.g., `cpu_metrics`,
`memory_metrics`, `disk_metrics`) that need to be combined for dashboards.
Continuous Aggregates can't span tables. A pg_trickle stream table can JOIN
and aggregate across them.

### 3.3. Hierarchical Pipelines Mixing Both

A Continuous Aggregate could provide a pre-materialized time-bucketed rollup
that feeds into a pg_trickle stream table for enrichment:

```
sensor_readings (hypertable)
  └─▶ hourly_readings (Continuous Aggregate — fast time-bucket rollup)
       └─▶ enriched_hourly_readings (pg_trickle stream table — JOIN with devices)
            └─▶ regional_summary (pg_trickle stream table — further aggregation)
```

### 3.4. Real-Time Alerting on Time-Series Data

pg_trickle's `NOTIFY`-based alerting combined with stream tables over
hypertables could enable threshold-based alerts that fire when an aggregated
metric crosses a boundary — without polling.

---

## 4. Engineering Challenges

### 4.1. Trigger-Based CDC on Hypertables — HIGH

Hypertables are the parent table; actual data lives in **chunk** child tables
(`_timescaledb_internal._hyper_<id>_<chunk_id>_chunk`). TimescaleDB redirects
INSERT operations to the appropriate chunk based on the time dimension.

**Impact:** Row-level `AFTER INSERT` triggers defined on the parent hypertable
**do** fire for inserts — TimescaleDB propagates triggers to chunks. However,
this behavior needs validation under:

- Chunk creation (new chunks appear as data arrives for new time ranges)
- Chunk compression (rows become read-only columnar data)
- Chunk decompression (for updates to compressed ranges)
- Chunk migration (move/copy between tablespaces)

**Investigation needed:**
- [ ] Verify trigger propagation works for newly created chunks
- [ ] Test that `pg_current_wal_lsn()` inside a trigger on a chunk returns a
      sensible value
- [ ] Benchmark trigger overhead on high-throughput hypertable inserts (time-
      series workloads often do 100K+ inserts/sec)

### 4.2. WAL-Based CDC on Hypertables — MEDIUM

Logical replication decoding sees DML on chunk tables, not the parent
hypertable. pg_trickle's WAL decoder would need to:

1. Map chunk OIDs back to the parent hypertable OID
2. Handle DDL from chunk management (CREATE TABLE for new chunks, ALTER for
   compression transitions)
3. Ignore or correctly handle the internal metadata changes that TimescaleDB
   writes alongside user data

The mapping can be done via `_timescaledb_catalog.chunk` →
`_timescaledb_catalog.hypertable` lookups, but this adds a dependency on
TimescaleDB's internal schema.

### 4.3. Compression Interactions — MEDIUM

When TimescaleDB compresses a chunk:
1. Data is READ from the uncompressed chunk
2. Compressed data is WRITTEN to a compressed chunk (internal format)
3. The uncompressed chunk is TRUNCATED

Steps 1–3 appear in WAL as normal DML. The DELETE + INSERT pattern would be
misinterpreted by pg_trickle's CDC as real data changes, triggering a spurious
full delta computation.

**Mitigation options:**
- Detect compression operations via `_timescaledb_catalog` metadata and skip
  the corresponding WAL events
- Use TimescaleDB's compression hooks (if exposed) to suppress CDC during
  compression
- Only track uncompressed (recent) chunks — this naturally aligns with the
  time-series pattern where only recent data changes

### 4.4. `time_bucket()` Function in Delta Queries — LOW

`time_bucket()` is a TimescaleDB function, not a core PostgreSQL function. When
pg_trickle parses a defining query containing `time_bucket()`, it needs to:

1. Recognize it as a deterministic, immutable function (safe for IVM)
2. Not attempt to "differentiate" through it — treat it as a scalar transform

pg_trickle already handles arbitrary immutable functions as pass-through in
delta queries, so this should work without changes. Needs validation.

### 4.5. Continuous Aggregate Internals as Source — LOW

If a user defines a stream table over a Continuous Aggregate's materialized
view, pg_trickle needs to handle the fact that the underlying object is a
materialized hypertable (with its own chunk structure), not a plain table.
Triggers on the cagg's materialized hypertable would capture the deltas that
TimescaleDB's materializer produces.

---

## 5. Design Ideas Worth Borrowing

### 5.1. Time-Bucketed Invalidation Tracking

**What TimescaleDB does:** Instead of tracking every changed row, the
invalidation log records which time-bucket ranges were modified:

```
invalidation_log: (hypertable_id, lowest_modified_value, greatest_modified_value)
```

On refresh, only the affected time buckets are recomputed — not the entire
table, and not individual row deltas.

**Applicability to pg_trickle:** For source tables with a time/sequence column
used in GROUP BY, pg_trickle could maintain a coarser invalidation log alongside
(or instead of) per-row change buffers. This would dramatically reduce change
buffer size for time-series-style workloads.

**Complexity:** HIGH — requires detecting temporal grouping patterns in the
defining query and implementing range-based invalidation in the refresh engine.

**Priority:** Phase 3+ (optimization, not correctness)

### 5.2. Real-Time Aggregation Mode

**What TimescaleDB does:** Continuous Aggregates support a "real-time" mode that
at query time combines:
- The materialized (potentially stale) aggregate data
- A live query over unmaterialized recent data (since last refresh)

The result is always-fresh reads without requiring a refresh.

**Applicability to pg_trickle:** A stream table could expose a view that
UNIONs:
1. The materialized storage table
2. A delta query over the unflushed change buffer

```sql
-- Conceptual: auto-generated view
CREATE VIEW st_realtime AS
    SELECT * FROM st_materialized
    UNION ALL
    SELECT * FROM (/* delta query over change buffer */);
```

**Complexity:** MEDIUM-HIGH — the delta query derivation already exists (it's
the refresh path). The challenge is making it performant at query time without
a full refresh cycle, and handling deduplication correctly for non-append-only
workloads (UPDATEs/DELETEs require MERGE-like semantics, not UNION ALL).

**Priority:** Phase 2 — high user value; aligns with the "freshness without
latency" goal.

**See also:** [PLAN_STREAMING_AGGREGATION.md](../sql/PLAN_STREAMING_AGGREGATION.md)
for the sub-second latency path, which overlaps with this idea.

### 5.3. Append-Only Fast Path

**What TimescaleDB does:** Time-series workloads are insert-only. TimescaleDB
optimizes for this by not tracking deletes/updates in the invalidation log.

**Applicability to pg_trickle:** When pg_trickle detects that a source table
is append-only (configurable hint or detected via trigger — no UPDATE/DELETE
events observed), the delta query can be simplified:

- **JOINs:** Skip the bilinear expansion. For `A JOIN B` where A is append-only
  and B is static, the delta is just `ΔA JOIN B` (one term instead of three).
- **Aggregates:** Skip maintaining DELETE counters in auxiliary state.
- **Change buffer:** Skip the `op` column (always 'I'); use a simpler schema.

**Complexity:** LOW-MEDIUM — this is a specialization of existing paths.

**Priority:** Phase 2 — significant performance win for time-series use cases.

### 5.4. Coarse-Grained Change Tracking for Partitioned Tables

**What TimescaleDB does:** Invalidation is tracked per-chunk (partition), not
per-row. Chunks map to time ranges, so "which chunks changed?" is a O(1) check
per chunk.

**Applicability to pg_trickle:** For PostgreSQL native partitioned tables (not
just hypertables), pg_trickle could maintain a partition-level change flag:

1. On first change to a partition since last refresh, set a dirty flag
2. On refresh, only scan change buffers for dirty partitions
3. Clear dirty flags after refresh

This doesn't change the delta computation — it's a filter that avoids scanning
empty change buffers.

**Complexity:** LOW — partition OIDs are available from `pg_inherits`.

**Priority:** Phase 2 — especially valuable when combined with TimescaleDB.

### 5.5. Declarative Policy Framework

**What TimescaleDB does:** Policies are first-class objects:

```sql
SELECT add_continuous_aggregate_policy('hourly_readings',
    start_offset => INTERVAL '3 hours',
    end_offset   => INTERVAL '1 hour',
    schedule_interval => INTERVAL '1 hour'
);
```

Policies have built-in monitoring (`timescaledb_information.job_stats`), retry
logic, exponential backoff, and can be paused/resumed.

**Applicability to pg_trickle:** pg_trickle already has schedule and status
management via `alter_stream_table`. The lesson is more about **UX**:

- Expose refresh policies as queryable objects (already done via
  `pgtrickle.pgt_stream_tables`)
- Add retry/backoff configuration (partially done via `consecutive_errors`)
- Consider a `pgtrickle.add_refresh_policy()` wrapper that mirrors the
  TimescaleDB ergonomics for users coming from that ecosystem

**Priority:** Phase 3 — ergonomic polish.

### 5.6. Chunk-Aware Retention

**What TimescaleDB does:** `add_retention_policy()` automatically drops chunks
older than a threshold.

**Applicability to pg_trickle:** Stream table change buffers grow over time.
A built-in retention policy for change buffers (drop change buffer entries older
than N refreshes or N hours) would prevent unbounded growth. This is partially
handled by the post-refresh cleanup, but a dedicated policy would be more
robust for error scenarios where refreshes fail repeatedly.

**Priority:** Phase 3.

---

## 6. Action Plan

### Phase 1 — Validation (1–2 weeks)

**Goal:** Confirm that pg_trickle works at all with hypertable source tables.

| # | Task | Effort |
|---|------|--------|
| 1.1 | Set up a test environment with both extensions on PG 18 | 2h |
| 1.2 | Create a hypertable and define a pg_trickle stream table over it (simple GROUP BY) | 2h |
| 1.3 | Verify trigger-based CDC fires correctly on hypertable inserts | 2h |
| 1.4 | Test with chunk creation (insert data spanning multiple time ranges) | 2h |
| 1.5 | Test with `time_bucket()` in defining query | 1h |
| 1.6 | Measure trigger overhead on high-throughput inserts (10K–100K rows/sec) | 4h |
| 1.7 | Document findings and any blockers | 2h |

**Deliverable:** Test report confirming basic compatibility or listing blockers.

### Phase 2 — Core Optimizations (2–4 weeks)

**Goal:** Implement the highest-value patterns learned from TimescaleDB.

| # | Task | Effort | Depends on |
|---|------|--------|------------|
| 2.1 | Append-only source hint (`source_mode := 'append_only'`) | 3d | — |
| 2.2 | Simplified delta queries for append-only JOINs | 3d | 2.1 |
| 2.3 | Partition-level dirty tracking for partitioned/hyper tables | 2d | Phase 1 |
| 2.4 | Investigate real-time aggregation mode (design doc) | 3d | — |
| 2.5 | E2E test suite: pg_trickle + TimescaleDB scenarios | 3d | Phase 1 |

### Phase 3 — Advanced Integration (4–8 weeks)

**Goal:** Handle edge cases and polish the combined experience.

| # | Task | Effort | Depends on |
|---|------|--------|------------|
| 3.1 | WAL decoder: map chunk OIDs → parent hypertable OID | 3d | Phase 1 |
| 3.2 | WAL decoder: filter compression-related DML | 2d | 3.1 |
| 3.3 | Stream table over Continuous Aggregate source | 3d | Phase 1 |
| 3.4 | Time-bucketed invalidation tracking (design + prototype) | 2w | 2.4 |
| 3.5 | Documentation: "Using pg_trickle with TimescaleDB" tutorial | 2d | Phase 2 |
| 3.6 | Refresh policy ergonomics (`add_refresh_policy` wrapper) | 2d | — |
| 3.7 | Change buffer retention policy | 2d | — |

---

## 7. Non-Goals

- **Replacing Continuous Aggregates.** For simple `time_bucket` GROUP BY on a
  single hypertable, Continuous Aggregates are faster and simpler. pg_trickle
  targets the cases caggs can't handle.
- **Depending on TimescaleDB.** All TimescaleDB integration must be optional.
  pg_trickle detects TimescaleDB at runtime (`SELECT 1 FROM pg_extension WHERE
  extname = 'timescaledb'`) and enables optimizations conditionally.
- **Modifying TimescaleDB internals.** All integration works through public
  APIs and catalog tables.

---

## 8. Open Questions

1. **Trigger propagation on new chunks** — Does TimescaleDB propagate existing
   parent-table triggers to chunks created *after* the trigger was defined?
   (Likely yes, but needs verification.)

2. **Compression hook** — Is there a way to detect that a chunk is being
   compressed (hook, event trigger, catalog flag) so CDC can ignore the
   resulting DML?

3. **Cagg materialized hypertable OID stability** — When a Continuous Aggregate
   is refreshed, does the materialized hypertable's OID remain stable? (Almost
   certainly yes, but worth confirming for CDC registration.)

4. **`time_bucket` immutability** — Is `time_bucket()` marked `IMMUTABLE` in
   the catalog? pg_trickle requires source functions to be immutable for
   correct IVM. (Likely yes for fixed-interval overloads; the timezone-aware
   overloads may be `STABLE`.)

5. **Licensing** — Some TimescaleDB features (e.g., compression, continuous
   aggregates) are under the Timescale License, not Apache-2.0. Does depending
   on their catalog tables or behavior create any licensing concerns for
   pg_trickle? (Likely no — pg_trickle would only query public catalog views,
   not link to TSL code.)

---

## 9. References

- [TimescaleDB documentation — Continuous Aggregates](https://docs.timescale.com/use-timescale/continuous-aggregates/)
- [TimescaleDB documentation — Hypertables](https://docs.timescale.com/use-timescale/hypertables/)
- [TimescaleDB documentation — Compression](https://docs.timescale.com/use-timescale/compression/)
- [TimescaleDB source — Invalidation log](https://github.com/timescale/timescaledb/blob/main/src/ts_catalog/continuous_agg.h)
- [DBSP paper (Budiu et al., 2022)](https://arxiv.org/abs/2203.16684)
- pg_trickle [ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
- pg_trickle [PLAN_STREAMING_AGGREGATION.md](../sql/PLAN_STREAMING_AGGREGATION.md)
- pg_trickle [PLAN_CITUS.md](../infra/PLAN_CITUS.md) — similar extension-compat plan
