# Performance & Scalability — New Feature Research Report

> Date: 2026-03-12
> Status: Research / Proposal
> Scope: High-level features for performance and scalability beyond current plans

---

## Executive Summary

This report identifies **12 high-level performance and scalability features**
not yet covered by existing plans (Parts 8–9, parallelization report, TPC-H
plan). Features are grouped into four themes:

1. **Reduce the MERGE bottleneck** — The 70–97% hotspot
2. **Smarter delta computation** — Do less work per refresh cycle
3. **Scale to larger deployments** — 100+ STs, multi-TB databases, multi-tenant
4. **Reduce CDC overhead** — Lighter change capture on the write path

Each feature includes a description, rationale, expected impact, effort
estimate, and references to prior art from Feldera, Noria/ReadySet, DBSP,
Flink, and academic literature.

---

## Theme A — Reduce the MERGE Bottleneck

Per Part 8 profiling, PostgreSQL `MERGE` execution dominates 70–97% of refresh
time. The MERGE statement joins the delta CTE against the stream table, then
performs INSERT/UPDATE/DELETE. Every feature in this theme attacks that cost
from a different angle.

### A-1: Partitioned Stream Tables (Partition-Scoped MERGE)

**Problem.** When a stream table has 10M+ rows, the MERGE scans the entire
target table even when the delta touches only a handful of partitions.
PostgreSQL's MERGE does support partition pruning, but only if the target is
declaratively partitioned and the join predicate aligns with the partition key.

**Proposal.** When the user defines a stream table with `PARTITION BY` on the
source or explicitly on the stream table:

1. Auto-create the stream table as a partitioned table (RANGE or LIST on the
   declared partition key).
2. At refresh time, inspect the delta to determine which partitions are
   affected (scan the delta CTE for `min/max` of the partition key).
3. Rewrite the MERGE to target only the affected partitions using a
   `WHERE partition_key BETWEEN ? AND ?` predicate that enables partition
   pruning.
4. For cold partitions (no changes), skip entirely — zero-cost.

**Expected Impact.** For a 10M-row stream table partitioned into 100
partitions with a 0.1% change rate concentrated in 2–3 partitions, the MERGE
scans ~100K rows instead of 10M — **100× reduction** in MERGE I/O.

**Effort.** 3–5 weeks (DDL changes, partition detection, MERGE rewrite).

**Prior Art.** Flink's partitioned state backends; TimescaleDB hypertable
chunk-scoped operations; Materialize's partitioned arrangements.

---

### A-2: Columnar Change Tracking (Column-Scoped Delta)

**Problem.** When a source table UPDATE changes only 1 of 50 columns, the
current CDC captures the entire row (old + new) and the delta query processes
all columns. If the changed column is not referenced by the stream table's
defining query, the entire refresh is wasted work.

**Proposal.** Track per-column change metadata:

1. In the CDC trigger, compute a bitmask of actually-changed columns
   (`old.col IS DISTINCT FROM new.col` for each column) and store it as an
   `int8` or `bit(n)` alongside the change row.
2. At delta-query build time, consult the bitmask:
   - If no referenced column changed → **skip the row entirely** (filter in
     the `delta_scan` CTE).
   - If only projected columns changed (no join keys, no filter predicates,
     no aggregate keys) → use a lightweight UPDATE-only path instead of
     full DELETE+INSERT.
3. For aggregates, if only the aggregated value column changed (not the GROUP
   BY key), emit a single correction row instead of two (delete old group +
   insert new group).

**Expected Impact.** In OLTP workloads where UPDATEs dominate and typically
touch 1–3 columns of wide tables: **50–90% reduction in delta row volume**
for wide-table scenarios.

**Effort.** 3–4 weeks (trigger changes, bitmask propagation, delta CTE
generation changes).

**Prior Art.** Oracle's column-change tracking in materialized view logs;
Feldera's per-field Z-set weights; DBToaster's multi-level deltas.

---

### A-3: MERGE Bypass via Direct INSERT OVERWRITE

**Problem.** PostgreSQL's MERGE planner can choose suboptimal join strategies,
especially for small deltas against large target tables. The MERGE also
imposes overhead from its three-way MATCHED/NOT MATCHED/NOT MATCHED BY SOURCE
logic.

**Proposal.** For specific operator tree shapes, bypass MERGE entirely:

1. **Append-Only Stream Tables.** When the defining query references only
   INSERT-only sources (e.g., event logs, append-only tables where CDC never
   sees DELETE/UPDATE), skip the MERGE entirely and use a simple
   `INSERT INTO st SELECT ... FROM delta`.

2. **Full-Replacement Window.** When the stream table has TopK semantics
   (ORDER BY + LIMIT) with a small limit (< 10K), use
   `TRUNCATE st; INSERT INTO st SELECT ... FROM (full_query) LIMIT N`.
   This is faster than MERGE when the target is small and the delta can
   recompute cheaply.

3. **Bulk COPY Path.** When the delta produces > 10K rows (high change rate
   but below adaptive-FULL threshold), switch to:
   ```
   DELETE FROM st WHERE __pgt_row_id IN (SELECT __pgt_row_id FROM delta WHERE action='D');
   COPY st FROM (SELECT ... FROM delta WHERE action='I');
   ```
   COPY is 2–5× faster than INSERT for bulk loads due to bypassing
   per-row executor overhead.

**Expected Impact.** 30–70% MERGE time reduction for qualifying scenarios.
Append-only path eliminates MERGE entirely for event-sourced architectures.

**Effort.** 2–3 weeks (path selection logic, template variants, benchmarks).

**Prior Art.** Snowflake's MERGE vs INSERT OVERWRITE cost-based selection;
DuckDB's bulk COPY path; Redshift's MERGE alternatives.

**Safety Analysis — Full-Replacement Window (TRUNCATE + INSERT).**

The `TRUNCATE st; INSERT INTO st ...` pattern for TopK tables is
**correct under PostgreSQL's MVCC** — external readers see the old
snapshot until the transaction commits, so there is no "empty table"
visibility window for concurrent sessions. The existing
`execute_full_refresh` path in `refresh.rs` already uses this pattern
and handles the required guard work:

- Sets `SET LOCAL pg_trickle.internal_refresh = 'true'` before TRUNCATE
  so DML-guard triggers allow the modification.
- Disables user triggers via `DISABLE TRIGGER USER` / `ENABLE TRIGGER USER`
  around the TRUNCATE + INSERT window.

However, the following concerns must be addressed before implementing
this path:

1. **`ACCESS EXCLUSIVE` lock starvation.** `TRUNCATE` acquires
   `ACCESS EXCLUSIVE` on the stream table, which blocks *all* concurrent
   readers (`SELECT`) for the entire duration including the INSERT phase.
   By contrast, MERGE acquires only row-level locks and allows reads to
   proceed against the pre-MERGE snapshot. For TopK tables refreshed
   sub-second this is a significant regression. **Mitigation:** gate
   the TRUNCATE path on a minimum `refresh_interval` (e.g., ≥ 5 s), or
   expose a GUC `pg_trickle.topk_bypass_mode = 'merge' | 'truncate'`
   so operators can opt in explicitly.

2. **Change-buffer LSN reset required.** After a TRUNCATE+INSERT full
   replacement, buffered change-buffer entries for the source table must
   not be applied in the next differential refresh — that would
   double-count them. `execute_full_refresh` avoids this because the
   scheduler resets the change-frontier LSN after a full refresh.
   The TRUNCATE path for TopK must trigger the same LSN reset; omitting
   it would silently corrupt the stream table.

3. **No-op guard missing.** TRUNCATE+INSERT always rewrites the table and
   holds `ACCESS EXCLUSIVE` even when the TopK result is unchanged.
   MERGE's `WHEN MATCHED AND (IS DISTINCT FROM ...)` clause naturally
   skips unchanged rows. The implementation should check whether the
   source delta contains any changes before issuing the TRUNCATE, or
   keep MERGE as the default and only fall back to TRUNCATE+INSERT when
   the delta exceeds a size threshold where MERGE wins.

4. **`__pgt_row_id` stability.** TRUNCATE+INSERT regenerates all row IDs.
   Since `row_id_expr_for_query` produces a deterministic content hash,
   IDs are stable across refreshes for unchanged rows. No correctness
   issue, but this should be verified by tests that assert row-ID
   continuity across a TRUNCATE-path refresh cycle.

5. **No RETURNING from TRUNCATE.** TRUNCATE has no `RETURNING` clause,
   so per-row counts for `pgtrickle_refresh` NOTIFY payloads and
   monitoring metrics must be derived from the INSERT's row count only.

---

### A-4: Index-Aware MERGE Planning

**Problem.** The MERGE joins delta against the stream table on `__pgt_row_id`.
If the stream table lacks an index on this column (it currently has one, but
the planner may not use it for small deltas), PostgreSQL may choose a
sequential scan.

**Proposal.**

1. **MERGE planner hints injection.** Extend `pg_trickle.merge_planner_hints`
   to automatically include:
   - `SET enable_seqscan = off` when `delta_row_count < 0.01 * target_count`
   - Index hints (PG 18 planner hints extension) pointing to the
     `__pgt_row_id` index.
2. **Covering index auto-creation.** When a stream table is created, if the
   defining query's output columns are small enough (< 8 columns), create a
   covering index `INCLUDE (col1, col2, ...)` on `__pgt_row_id` to enable
   index-only scans during MERGE. This eliminates the heap-fetch for matched
   rows.
3. **Partial index for hot partitions.** For time-series stream tables, create
   a partial index on `__pgt_row_id WHERE updated_at > now() - interval '1 day'`
   to keep the index small and hot.

**Expected Impact.** 20–50% MERGE time reduction for small-delta/large-target
scenarios where the planner currently chooses seq-scan.

**Effort.** 1–2 weeks (planner hint logic, auto-index DDL).

**Prior Art.** pg_hint_plan; Oracle's materialized view log indexes;
SQL Server's indexed views with clustered index selection.

---

## Theme B — Smarter Delta Computation

These features reduce the amount of work the DVM engine and PostgreSQL do
per refresh cycle, independent of the MERGE step.

### B-1: Incremental Aggregate Maintenance (Algebraic Shortcuts)

**Problem.** The current aggregate delta rule recomputes entire groups where
the GROUP BY key appears in the delta. For a group with 100K rows where 1 row
changed, the aggregate re-scans all 100K rows in that group.

**Proposal.** Implement algebraic aggregate maintenance for decomposable
aggregates:

| Aggregate | Algebraic Maintenance | Aux State |
|-----------|-----------------------|-----------|
| COUNT     | `old_count + Δcount` | Single counter |
| SUM       | `old_sum + Δsum` | Single accumulator |
| AVG       | `(old_sum + Δsum) / (old_count + Δcount)` | Sum + count |
| MIN/MAX   | If deleted value ≠ current min/max → no rescan | Current min/max + count-at-min |
| STDDEV    | Online Welford update | Mean + M2 + count |

Implementation:
1. Add auxiliary columns to the stream table: `__pgt_aux_count`,
   `__pgt_aux_sum`, etc. (hidden from user queries via view wrapper).
2. Delta query emits correction values instead of full group recomputation:
   ```sql
   UPDATE st SET
     total_amount = total_amount + delta.sum_amount,
     row_count = row_count + delta.count_change,
     avg_amount = (total_amount + delta.sum_amount) /
                  NULLIF(row_count + delta.count_change, 0)
   WHERE st.group_key = delta.group_key
   ```
3. Fall back to full-group recomputation for non-decomposable aggregates
   (mode, percentile, string_agg with ordering).

**Expected Impact.** For high-cardinality GROUP BY with large groups:
**10–1000× speedup** per aggregate group (from O(group_size) to O(1)).
Benchmarks show aggregate scenarios at 100K/1% go from 2.5ms to sub-1ms.

**Effort.** 4–6 weeks (auxiliary column management, algebraic rules per
aggregate, fallback logic, correctness tests).

**Prior Art.** DBSP's linear operator lifting; DBToaster's higher-order
maintenance; Feldera's in-memory aggregate state; Noria's partial state.

**Risk.** Auxiliary columns increase storage. Need migration story for
existing stream tables. Floating-point aggregates may accumulate rounding
errors over many cycles (periodic recomputation to reset).

---

### B-2: Delta Predicate Pushdown

**Problem.** The delta CTE chain processes all changes, then filters. For a
query like `SELECT ... FROM orders WHERE status = 'shipped'`, if a change row
has `status = 'pending'`, the delta processes it through scan → filter → discard.
The scan and any join work is wasted.

**Proposal.** Push WHERE predicates from the defining query down into the
change buffer scan:

1. During OpTree construction, identify `Filter` nodes whose predicates
   reference only columns from a single source table.
2. Inject these predicates into the `delta_scan` CTE as additional WHERE
   clauses:
   ```sql
   -- Before: scans all changes, filters later
   delta_scan_orders AS (
     SELECT * FROM pgtrickle_changes.changes_12345
     WHERE lsn BETWEEN ? AND ?
   )

   -- After: filters at scan time
   delta_scan_orders AS (
     SELECT * FROM pgtrickle_changes.changes_12345
     WHERE lsn BETWEEN ? AND ?
       AND (new_status = 'shipped' OR old_status = 'shipped')
   )
   ```
3. The `OR old_status = 'shipped'` handles deletions from the filter's
   qualifying set (a row that was 'shipped' and is now 'pending' must be
   removed from the stream table).

**Expected Impact.** For selective queries (< 10% selectivity),
**5–10× reduction in delta row volume** before any join processing.

**Effort.** 2–3 weeks (predicate extraction, pushdown rules, correctness
for UPDATE scenarios where old/new may differ).

**Prior Art.** Standard RDBMS predicate pushdown; Flink's filter pushdown
into source connectors; Materialize's predicate pushdown through arrangements.

---

### B-3: Multi-Table Delta Batching

**Problem.** When a stream table joins tables A, B, and C, and all three have
changes in the same cycle, the current delta query generates:
```
ΔQ = (ΔA ⋈ B₁ ⋈ C₁) ∪ (A₀ ⋈ ΔB ⋈ C₁) ∪ (A₀ ⋈ B₀ ⋈ ΔC)
```
This requires 3 separate passes through the source tables with different
snapshots.

**Proposal.** Optimize multi-source delta by:

1. **Simultaneous delta merging.** When Δ is small relative to base tables,
   precompute a merged delta:
   ```sql
   merged_delta AS (
     SELECT 'A' AS src, * FROM delta_A
     UNION ALL
     SELECT 'B' AS src, * FROM delta_B
     UNION ALL
     SELECT 'C' AS src, * FROM delta_C
   )
   ```
   Then run a single join pass that handles all source changes together.

2. **Change-ordering optimization.** Within a single cycle, if table A has 0
   changes but B has 100, skip the A-delta branch entirely (already
   implemented via no-change short-circuit), but extend this to skip
   individual UNION ALL branches within a multi-source delta.

3. **Cross-delta deduplication.** When ΔA and ΔB produce the same output row
   (e.g., both sides of a join changed), the current approach may produce
   duplicate corrections. Add a final `DISTINCT ON (__pgt_row_id)` only when
   the optimizer detects diamond-shaped delta flow.

**Expected Impact.** For multi-join queries with changes in multiple sources:
**30–50% reduction in total I/O** by eliminating redundant base-table scans.

**Effort.** 4–6 weeks (delta branch pruning, merged delta generation,
correctness proofs for simultaneous changes).

**Prior Art.** DBSP's simultaneous delta processing; DBToaster's factorized
evaluation; Materialize's shared arrangements.

---

### B-4: Cost-Based Refresh Strategy Selection

**Problem.** The adaptive FULL/DIFFERENTIAL threshold is currently a fixed
ratio (`pg_trickle.differential_max_change_ratio` default 0.5). This is a
blunt instrument — a join-heavy query may be better off with FULL at 5% change
rate, while a scan-only query benefits from DIFFERENTIAL up to 80%.

**Proposal.** Replace the fixed threshold with a cost-based optimizer:

1. **Collect statistics per ST.** After each refresh, record:
   - `delta_row_count` (already tracked)
   - `merge_duration_ms` (already tracked)
   - `full_refresh_duration_ms` (tracked from FULL refreshes)
   - `target_row_count` (already tracked)
   - `query_complexity_class` (scan/filter/aggregate/join/join_agg)

2. **Build a cost model.** Using historical data from `pgt_refresh_history`:
   ```
   estimated_diff_cost(Δ) = α × Δ_rows + β × target_rows + γ × complexity
   estimated_full_cost    = δ × target_rows + ε × complexity
   ```
   Fit α, β, γ, δ, ε via linear regression on the last N refreshes.

3. **Select strategy per cycle.** Before each refresh:
   ```
   IF estimated_diff_cost(current_Δ) < estimated_full_cost × safety_margin:
     USE DIFFERENTIAL
   ELSE:
     USE FULL
   ```
   Safety margin (default 0.8) ensures we prefer DIFFERENTIAL even when costs
   are close, since it has lower lock contention.

4. **Cold-start heuristic.** Before enough data is collected (< 10 refreshes),
   use the current fixed threshold as fallback.

**Expected Impact.** Eliminates the join_agg 100K/10% regression (0.3×) by
correctly falling back to FULL. Improves scan/filter scenarios by allowing
DIFFERENTIAL at higher change rates. **5–20% overall throughput improvement**
across mixed workloads.

**Effort.** 2–3 weeks (statistics collection, cost model, strategy selector).

**Prior Art.** PostgreSQL's query planner cost model; Oracle's cost-based
refresh decision for materialized views; SQL Server's indexed-view maintenance
cost estimation.

---

## Theme C — Scale to Larger Deployments

These features address operational scalability: supporting 100+ stream tables,
multi-TB databases, and multi-tenant environments.

### C-1: Tiered Refresh Scheduling (Hot/Warm/Cold)

**Problem.** Current scheduling treats all STs equally — each runs on its
configured schedule regardless of how stale the data actually is or how
frequently it's queried. In a deployment with 500 STs, refreshing all of them
every few seconds wastes CPU on STs that nobody is reading.

**Proposal.** Implement demand-driven tiered scheduling:

1. **Query tracking.** Use `pg_stat_user_tables` to track `seq_scan` and
   `idx_scan` counts on each stream table. Periodically sample these counters
   to determine read frequency.

2. **Tier classification:**
   | Tier | Read Frequency | Refresh Strategy |
   |------|---------------|-----------------|
   | **Hot** | > 10 reads/min | Refresh at configured schedule |
   | **Warm** | 1–10 reads/min | Refresh at 2× configured schedule |
   | **Cold** | < 1 read/min | Refresh only on-demand (lazy) |
   | **Frozen** | 0 reads in last hour | Skip entirely; refresh on first read |

3. **Lazy refresh trigger.** For Cold/Frozen STs, install a lightweight
   `pg_stat_user_tables` polling check. When a read is detected, queue an
   immediate refresh. Optionally, use PostgreSQL's `track_io_timing` or a
   custom hook to detect the first sequential scan.

4. **Configurable per-ST.** Allow `ALTER STREAM TABLE ... SET (tier = 'hot')`
   to override automatic classification.

**Expected Impact.** In a 500-ST deployment where 80% are Cold/Frozen:
**80% reduction in scheduler CPU** and background worker utilization.
Hot STs see no degradation.

**Effort.** 3–4 weeks (tier classification, lazy refresh trigger, scheduler
integration).

**Prior Art.** Materialize's on-demand clusters; Oracle's query-driven refresh;
Flink's idle source detection.

**Risk.** Lazy refresh introduces latency on first read after changes. Need a
`pg_trickle.lazy_refresh_timeout` GUC to bound maximum read latency.

---

### C-2: Incremental DAG Rebuild

**Problem.** When any DDL change occurs (detected via `DAG_REBUILD_SIGNAL`),
the entire dependency graph is rebuilt from scratch by querying
`pgt_dependencies`. For 1000+ STs this becomes expensive (O(V+E) SPI queries).

**Proposal.** Implement incremental DAG maintenance:

1. **Delta-based rebuild.** When `DAG_REBUILD_SIGNAL` fires, record which
   specific ST was affected (store the `pgt_id` in shared memory alongside
   the signal counter).
2. Add/remove only the affected edges and vertices, then re-run topological
   sort only on the affected subgraph.
3. Cache the sorted schedule in shared memory (currently recomputed each
   cycle).

**Expected Impact.** DAG rebuild from O(V+E) to O(Δ_V + Δ_E) — typically
O(1) for single-ST DDL changes. Reduces scheduler latency spike from
~50ms to ~1ms at 1000 STs.

**Effort.** 2–3 weeks (incremental topo-sort, shared memory changes).

**Prior Art.** Flink's incremental job graph updates; Spark's adaptive query
re-optimization.

---

### C-3: Multi-Database Scheduler Isolation

**Problem.** The current launcher spawns one scheduler per database, but all
schedulers share a single cluster-wide worker budget
(`pg_trickle.max_dynamic_refresh_workers`). In multi-tenant deployments, one
busy database can starve others.

**Proposal.**

1. **Per-database worker quotas.** Add
   `pg_trickle.per_database_worker_quota` (default: equal share of
   `max_dynamic_refresh_workers`). Allow DBA to configure per-database:
   ```sql
   ALTER DATABASE analytics SET pg_trickle.worker_quota = 8;
   ALTER DATABASE reporting SET pg_trickle.worker_quota = 2;
   ```

2. **Priority-based scheduling.** When worker demand exceeds budget, use
   priority ordering:
   - Priority 1: STs with IMMEDIATE mode (transactional consistency)
   - Priority 2: Hot-tier STs (high read frequency)
   - Priority 3: Warm-tier STs
   - Priority 4: Cold-tier STs

3. **Burst capacity.** Allow databases to temporarily exceed their quota
   (up to 150%) if other databases are under-utilizing theirs. Reclaim
   burst capacity within 1 scheduler cycle.

**Expected Impact.** Prevents noisy-neighbor problems in multi-tenant
deployments. Ensures SLA compliance for high-priority databases.

**Effort.** 2–3 weeks (quota tracking in shared memory, priority queue,
burst logic).

**Prior Art.** Kubernetes resource quotas; YARN capacity scheduler;
PostgreSQL's per-database connection limits.

---

### C-4: Change Buffer Compaction

**Problem.** The change buffer tables (`pgtrickle_changes.changes_<oid>`) grow
unboundedly between refresh cycles. For high-write tables with long refresh
intervals, buffers can reach millions of rows, making the delta scan expensive.
Additionally, UPDATE operations produce two rows (DELETE old + INSERT new),
doubling the buffer size.

**Proposal.** Implement periodic compaction of change buffers:

1. **Row-level compaction.** Between refresh cycles, merge multiple changes to
   the same row ID into a single net change:
   - INSERT + DELETE = no-op (remove both)
   - INSERT + UPDATE = INSERT (with final values)
   - UPDATE + UPDATE = single UPDATE (with final values)
   - UPDATE + DELETE = DELETE (with original values)

2. **Trigger.** Run compaction when buffer size exceeds
   `pg_trickle.compact_threshold` (default: 100K rows per buffer) or at a
   configurable interval.

3. **Implementation.** Use a single SQL statement:
   ```sql
   WITH ranked AS (
     SELECT *, ROW_NUMBER() OVER (
       PARTITION BY __pgt_row_id ORDER BY lsn DESC
     ) AS rn
   FROM changes_<oid>
   WHERE lsn >= frontier_lsn
   )
   DELETE FROM changes_<oid> WHERE ctid NOT IN (
     SELECT ctid FROM ranked WHERE rn = 1
   )
   ```

4. **Net-zero detection.** When compaction discovers that a row was inserted
   and then deleted within the same cycle, eliminate both rows — the refresh
   need not process them at all.

**Expected Impact.** For high-churn tables with long refresh intervals:
**50–90% reduction in change buffer size**, proportional reduction in delta
scan time. Major benefit for `join` scenarios where delta size dominates.

**Effort.** 2–3 weeks (compaction logic, trigger integration, concurrency
safety).

**Prior Art.** LSM-tree compaction (RocksDB, LevelDB); Kafka log compaction;
Materialize's compaction of arrangements; HBase major/minor compaction.

**Risk.** Compaction itself has a cost. Must ensure it doesn't run during an
active refresh cycle (use advisory locks). Compaction on UNLOGGED tables is
simpler but loses crash safety for intermediate states.

---

## Theme D — Reduce CDC Overhead

These features target the write-side cost of change data capture, reducing the
impact of pg_trickle on OLTP write throughput.

### D-1: UNLOGGED Change Buffers

**Problem.** Change buffer tables are currently logged (WAL-written). Every
INSERT into a change buffer generates WAL records, doubling the WAL volume
for the source table's writes. This is especially painful for high-write
workloads.

**Proposal.** Make change buffer tables UNLOGGED:

1. Create change buffers as `CREATE UNLOGGED TABLE pgtrickle_changes.changes_<oid>`.
2. After a crash, change buffers are empty (PostgreSQL truncates UNLOGGED
   tables on recovery). This is acceptable because:
   - The next refresh cycle will fall back to FULL mode (no changes = no
     frontier advancement, triggering reinit).
   - Stream table data is still intact (it's a regular logged table).
3. Add a `pg_trickle.unlogged_buffers` GUC (default: `true`) to control this.

**Expected Impact.** **~30% reduction in WAL volume** from CDC triggers.
For write-heavy workloads (> 10K writes/sec), this translates to measurably
lower replication lag and checkpoint pressure.

**Effort.** 1–2 weeks (DDL changes, crash recovery handling, GUC).

**Prior Art.** Mentioned in PLAN_TPC_H_BENCHMARKING.md as O-3. PostgreSQL's
UNLOGGED tables; Oracle's NOLOGGING mode for materialized view logs.

**Risk.** After crash, one FULL refresh per ST is required. For most workloads
this is acceptable. Users who need crash-safe change buffers can set
`pg_trickle.unlogged_buffers = false`.

---

### D-2: Async CDC via Logical Decoding Output Plugin

**Problem.** The current WAL CDC mode uses `pgoutput` (standard logical
replication protocol) which requires a replication slot and a background
worker polling for changes. The polling interval introduces latency and the
replication slot retains WAL segments.

**Proposal.** Develop a custom logical decoding output plugin
(`pg_trickle_decoder`) that:

1. **Direct buffer writes.** Instead of decoding WAL → logical messages →
   polling → buffer insert, decode WAL directly into the change buffer table
   in a single step within the output plugin callback.
2. **Filtered decoding.** Only decode tables that are sources of active stream
   tables (skip all other tables in the WAL stream). This reduces CPU from
   decoding irrelevant tables.
3. **Batched output.** Accumulate decoded rows and flush in batches of 1000+
   rows using `SPI_exec` within the plugin, amortizing per-row overhead.

**Expected Impact.** **50–80% reduction in WAL decoding CPU** compared to
pgoutput polling. Eliminates the polling latency (changes appear in buffer
within ~10ms of WAL write instead of at the next poll interval).

**Effort.** 6–8 weeks (C-based output plugin, pgrx integration, testing).

**Prior Art.** wal2json, pgoutput, test_decoding (PostgreSQL built-in);
Debezium's custom output plugins; pg_logical's direct decoding.

**Risk.** High complexity. C code in PostgreSQL output plugin requires careful
memory management. Must handle transactions that span multiple WAL segments.
Consider as a v1.0+ feature.

---

### D-3: Write-Batched CDC Triggers

**Problem.** Even with statement-level triggers (v0.4.0), each DML statement
invokes the trigger function once. For COPY operations that insert 100K rows,
the trigger processes all 100K transition-table rows in a single function call,
which is good — but the trigger overhead is still per-statement.

**Proposal.** Optimize the CDC trigger function for batch scenarios:

1. **Bulk COPY path.** Detect when the trigger is processing > 1000 rows
   (via `SELECT count(*) FROM new_table`) and switch to a bulk-optimized
   path:
   ```sql
   INSERT INTO pgtrickle_changes.changes_<oid>
   SELECT nextval('...'), pg_current_wal_lsn(), 'I',
          <pk_cols>, <tracked_cols>
   FROM new_table;
   ```
   Skip per-row hashing and row-ID computation; use a set-returning insert.

2. **Deferred hashing.** For bulk operations, defer content hashing to
   refresh time instead of trigger time. Store raw rows in the change buffer;
   compute hashes lazily when the delta query runs.

3. **Adaptive trigger complexity.** For narrow tables (< 5 columns) with
   simple PKs, use a minimal trigger function that skips JSON serialization
   and stores typed columns directly.

**Expected Impact.** **20–40% reduction in trigger overhead** for bulk DML
operations. Larger gains for wide tables.

**Effort.** 2–3 weeks (trigger function variants, adaptive selection).

**Prior Art.** pgaudit's minimal-overhead logging; Debezium's batched
snapshot mode.

---

### D-4: Shared Change Buffers (Multi-ST Deduplication)

**Problem.** When multiple stream tables reference the same source table,
each has its own change buffer. A single INSERT into the source table triggers
N independent change buffer writes (one per dependent ST). For 10 STs
referencing the same source, write amplification is 10×.

**Proposal.** Share change buffers across STs that reference the same source:

1. **Single buffer per source.** Instead of `changes_<st_oid>`, use
   `changes_<source_oid>` (already partially implemented in CDC mode).
2. **Reference counting.** Track which STs have consumed changes via the
   frontier (already tracked per-ST in `pgt_dependencies.decoder_confirmed_lsn`).
3. **Cleanup coordination.** Only delete change buffer rows when ALL
   dependent STs have advanced their frontier past them.
4. **Column superset.** The shared buffer stores the union of columns needed
   by all dependent STs. Each ST's delta scan projects only the columns it
   needs.

**Expected Impact.** For N STs referencing the same source:
**N× reduction in change buffer write volume**, proportional reduction in
trigger overhead and WAL volume.

**Effort.** 3–4 weeks (buffer sharing logic, multi-frontier cleanup,
column superset management).

**Prior Art.** Kafka's shared topics with consumer groups;
Materialize's shared arrangements; Flink's shared source state.

---

## Prioritization Matrix

| ID | Feature | Impact | Effort | Risk | Priority |
|----|---------|--------|--------|------|----------|
| D-1 | UNLOGGED Change Buffers | Medium | 1–2 wk | Low | **P0** |
| A-3 | MERGE Bypass (INSERT OVERWRITE) | High | 2–3 wk | Low | **P0** |
| B-2 | Delta Predicate Pushdown | High | 2–3 wk | Low | **P1** |
| B-4 | Cost-Based Refresh Strategy | Medium | 2–3 wk | Low | **P1** |
| A-2 | Columnar Change Tracking | High | 3–4 wk | Medium | **P1** |
| C-4 | Change Buffer Compaction | High | 2–3 wk | Medium | **P1** |
| D-4 | Shared Change Buffers | High | 3–4 wk | Medium | **P2** |
| A-4 | Index-Aware MERGE Planning | Medium | 1–2 wk | Low | **P2** |
| C-1 | Tiered Refresh Scheduling | High | 3–4 wk | Medium | **P2** |
| B-1 | Incremental Aggregate Maintenance | Very High | 4–6 wk | High | **P2** |
| A-1 | Partitioned Stream Tables | Very High | 3–5 wk | High | **P3** |
| C-2 | Incremental DAG Rebuild | Medium | 2–3 wk | Low | **P3** |
| C-3 | Multi-DB Scheduler Isolation | Medium | 2–3 wk | Low | **P3** |
| B-3 | Multi-Table Delta Batching | High | 4–6 wk | High | **P3** |
| D-2 | Async CDC Output Plugin | High | 6–8 wk | High | **P4** |
| D-3 | Write-Batched CDC Triggers | Medium | 2–3 wk | Low | **P3** |

### Recommended Implementation Order

**Wave 1 (Quick Wins — v0.5.0):**
- D-1: UNLOGGED Change Buffers ← Already planned (O-3 in TPC-H plan)
- A-3: MERGE Bypass for append-only and TopK scenarios
- A-4: Index-Aware MERGE Planning

**Wave 2 (Core Optimizations — v0.6.0):**
- B-2: Delta Predicate Pushdown
- B-4: Cost-Based Refresh Strategy Selection
- C-4: Change Buffer Compaction

**Wave 3 (Scalability — v0.7.0):**
- A-2: Columnar Change Tracking
- C-1: Tiered Refresh Scheduling
- D-4: Shared Change Buffers

**Wave 4 (Advanced — v0.8.0+):**
- B-1: Incremental Aggregate Maintenance
- A-1: Partitioned Stream Tables
- B-3: Multi-Table Delta Batching
- C-2: Incremental DAG Rebuild
- C-3: Multi-DB Scheduler Isolation
- D-2: Custom Logical Decoding Output Plugin

---

## Competitive Positioning

These features, if implemented, would give pg_trickle significant advantages
over every competing system:

| Feature | pg_trickle (proposed) | Feldera | Epsio | pg_ivm |
|---------|----------------------|---------|-------|--------|
| Algebraic aggregate maint. | ✅ (B-1) | ✅ (native) | Partial | ❌ |
| Predicate pushdown in delta | ✅ (B-2) | ✅ (native) | Unknown | ❌ |
| Cost-based strategy | ✅ (B-4) | N/A (streaming) | N/A | ❌ |
| Partitioned targets | ✅ (A-1) | N/A | ❌ | ❌ |
| Column-scoped delta | ✅ (A-2) | Partial | ❌ | ❌ |
| Change buffer compaction | ✅ (C-4) | ✅ (native) | Unknown | N/A |
| Tiered scheduling | ✅ (C-1) | N/A | N/A | N/A |
| UNLOGGED buffers | ✅ (D-1) | N/A | N/A | N/A |
| Shared change buffers | ✅ (D-4) | ✅ (native) | Unknown | N/A |
| Custom WAL decoder | ✅ (D-2) | N/A | ✅ | ❌ |

The combination of **embedded PostgreSQL architecture** (zero external
dependencies) + **comprehensive SQL coverage** (39+ aggregates, full window
functions) + **these optimizations** would make pg_trickle the most capable
IVM system available for PostgreSQL.

---

## References

1. Budiu et al., "DBSP: Automatic Incremental View Maintenance," VLDB 2023
2. Koch et al., "DBToaster: Higher-Order Delta Processing for Dynamic,
   Frequently Fresh Views," VLDB Journal 2014
3. Gupta & Mumick, "Maintenance of Materialized Views: Problems, Techniques,
   and Applications," IEEE Data Engineering Bulletin 1995
4. Gjengset et al., "Noria: Dynamic, Partially-Stateful Data-Flow for
   High-Performance Web Applications," OSDI 2018
5. Abadi et al., "Materialize: A Streaming Database," CIDR 2023
6. McSherry et al., "Differential Dataflow," CIDR 2013
7. Nikolic et al., "LINVIEW: Incremental View Maintenance for Complex
   Analytical Queries," SIGMOD 2014
8. Chirkova & Yang, "Materialized Views," Foundations and Trends in
   Databases 2012
9. Oracle, "Materialized View Refresh: Fast, Complete, Force, and
   On-Demand," Oracle Database Data Warehousing Guide
10. PostgreSQL 18, "CREATE MATERIALIZED VIEW / REFRESH MATERIALIZED VIEW"
