# Parallelization Options for pg_trickle

> **Status:** Planning — no implementation changes proposed yet.
> **Date:** 2026-02-26
> **Related:** [ARCHITECTURE.md § 8](../../docs/ARCHITECTURE.md) ·
> [GAP_SQL_PHASE_7.md § G8.5](../sql/GAP_SQL_PHASE_7.md) ·
> [CONFIGURATION.md](../../docs/CONFIGURATION.md)

---

## 1. Current Architecture (Baseline)

pg_trickle registers **one PostgreSQL background worker** — the *scheduler* —
during `_PG_init()`. It wakes every `pg_trickle.scheduler_interval_ms`
(default 1 000 ms), rebuilds the dependency DAG when the catalog changes, and
walks stream tables (STs) in topological order, refreshing each one **inline
and sequentially** within the same process.

```
                    ┌───────────────────────────────────────┐
                    │       pg_trickle scheduler (1 BGW)     │
                    │                                       │
                    │  for st in topological_order():       │
                    │      if schedule_due(st):             │
                    │          acquire advisory_lock(st)    │
                    │          execute_refresh(st)  ◄─ blocking
                    │          release advisory_lock(st)    │
                    │                                       │
                    └───────────────────────────────────────┘
```

### What exists today

| Component | Status |
|---|---|
| `pg_trickle.max_concurrent_refreshes` GUC (default 4, max 32) | **Defined but not enforced for parallel dispatch.** Only prevents a manual `refresh_stream_table()` call from overlapping with the scheduler on the *same* ST via advisory locks. |
| Advisory lock infrastructure (`pg_try_advisory_lock` / `pg_advisory_unlock`) | Fully operational. Each refresh holds a per-ST advisory lock for its duration. Used for collision detection, not parallelism. |
| DAG topological ordering (Kahn's algorithm) | Produces a flat `Vec<NodeId>` — upstream before downstream. Does not expose parallelism levels. |
| Shared memory (`PgTrickleSharedState`, `DAG_REBUILD_SIGNAL`, `CACHE_GENERATION`) | Atomics for catalog-change signalling and cache invalidation. No worker-coordination primitives. |

### Consequence

If a deployment has 50 STs and each refresh takes 200 ms, a single scheduler
cycle takes ≥ 10 s even though many STs are independent and *could* run
concurrently.

### PostgreSQL resource budget

`max_worker_processes` (default 8) is the server-wide ceiling for **all**
background workers — autovacuum launchers, parallel query workers, logical
replication workers, and extension workers. pg_trickle currently consumes
**one** slot. Any parallelization approach that spawns additional workers must
account for this shared budget.

---

## 2. Parallelization Options

### Option A — Dynamic Background Workers (recommended first step)

#### Concept

The scheduler remains the single coordinator but **delegates refresh
execution to short-lived dynamic background workers** instead of running them
inline. pgrx exposes `BackgroundWorkerBuilder` which can register workers at
runtime with `BgWorkerStartTime::ConsistentState`.

```
                ┌──────────────────────────────────────────────┐
                │       pg_trickle scheduler (coordinator)      │
                │                                              │
                │  for st in topological_order():              │
                │      if schedule_due(st):                    │
                │          if active_workers < max_concurrent: │
                │              spawn_refresh_worker(st) ──┐    │
                │          else:                          │    │
                │              defer to next cycle        │    │
                │                                         │    │
                │  poll / reap completed workers           │    │
                └─────────────────────────────────────────│────┘
                                                          │
                     ┌──────────────┐  ┌──────────────┐   │
                     │ worker st_a  │  │ worker st_b  │ ◄─┘
                     │ (refresh)    │  │ (refresh)    │
                     └──────────────┘  └──────────────┘
```

#### Implementation sketch

1. **Worker entry point** — A new function
   `pg_trickle_refresh_worker_main(datum)` receives `pgt_id` via the
   `Datum` argument, acquires the advisory lock, runs the refresh, stores
   results in shared memory or a catalog table, and exits.

2. **Coordinator dispatch** — In the main loop, instead of calling
   `execute_scheduled_refresh(st)`, the scheduler calls
   `spawn_refresh_worker(st.pgt_id)`. It tracks active workers in a
   `HashMap<i64, BackgroundWorkerHandle>`.

3. **Worker completion** — After dispatching all eligible STs in a
   topological level, the scheduler calls `wait_latch()` until at least one
   worker finishes (detected via shared-memory flag or `WaitForBackgroundWorkerShutdown`).

4. **Topological barrier** — STs in level N+1 are not dispatched until all
   level-N workers have finished (see Option B for level extraction).

5. **Respect `max_concurrent_refreshes`** — The coordinator caps the number
   of simultaneously spawned workers to `pg_trickle.max_concurrent_refreshes`.

#### Pros

- True OS-level parallelism — each worker has its own backend, transaction,
  and SPI connection.
- Natural fit for the existing advisory-lock infrastructure (each worker
  locks its own ST).
- `max_concurrent_refreshes` GUC becomes meaningful.
- Error isolation — a crashing worker is reaped without killing the
  scheduler.

#### Cons

- Each dynamic worker consumes a `max_worker_processes` slot for the
  duration of the refresh (typically milliseconds to seconds).
- Retry state currently lives in-memory inside the scheduler
  (`HashMap<i64, RetryState>`). Workers would need to communicate outcomes
  back (shared memory, catalog table, or pipe).
- pgrx `BackgroundWorkerBuilder` dynamic registration requires careful
  lifetime handling — the library name and function name must be
  null-terminated statics.
- More complex crash-recovery logic: if the scheduler crashes while workers
  are running, the workers may outlive it.

#### Effort estimate

12–16 hours.

#### Risks

| Risk | Mitigation |
|---|---|
| `max_worker_processes` exhaustion | Check available slots before spawning; fall back to inline execution if none available. |
| Worker outlives scheduler | Workers should check their parent PID or use a latch. PostgreSQL terminates orphan workers on postmaster restart. |
| Shared memory for result reporting | Use an atomic array indexed by slot, or write to `pgt_refresh_history` from within the worker (already done). |

---

### Option B — Level-Parallel DAG Dispatch

#### Concept

A refinement of Option A. Instead of walking the topological order as a flat
list, partition STs into **parallelism levels** — groups of STs with no
dependency edges between them. All STs within a level can run concurrently;
levels execute sequentially.

```
DAG example:

    base_orders ─┬─► st_daily_agg ──► st_weekly_rollup
                 │
    base_users ──┴─► st_user_stats ──► st_dashboard
                 │
                 └─► st_user_segments

Level 0:  [st_daily_agg, st_user_stats, st_user_segments]   ← parallel
Level 1:  [st_weekly_rollup, st_dashboard]                   ← parallel
```

#### Implementation sketch

1. **Level extraction** — Modify `StDag::topological_order()` to return
   `Vec<Vec<NodeId>>` (levels) instead of `Vec<NodeId>`. This is a trivial
   change to Kahn's algorithm — instead of appending all dequeued nodes to a
   single list, collect each "wave" of zero-indegree nodes as a level.

   ```rust
   pub fn topological_levels(&self) -> Result<Vec<Vec<NodeId>>, PgTrickleError> {
       // ... existing Kahn setup ...
       let mut levels = Vec::new();
       while !queue.is_empty() {
           let mut level = Vec::new();
           for _ in 0..queue.len() {
               let node = queue.pop_front().unwrap();
               for &target in self.edges.get(&node).unwrap_or(&vec![]) {
                   let deg = in_degree.get_mut(&target).unwrap();
                   *deg -= 1;
                   if *deg == 0 {
                       next_queue.push_back(target);
                   }
               }
               level.push(node);
           }
           levels.push(level);
           std::mem::swap(&mut queue, &mut next_queue);
       }
       Ok(levels)
   }
   ```

2. **Scheduler loop** — For each level, dispatch all eligible STs in
   parallel (up to `max_concurrent_refreshes`), then wait for all to
   complete before moving to the next level.

#### Pros

- Maximizes parallelism while **guaranteeing** dependency ordering.
- Level extraction is a ~30-line change to the existing Kahn's algorithm.
- Composes naturally with Option A (dynamic workers execute each level's
  STs in parallel).

#### Cons

- Late-binding of levels to workers may leave some workers idle if one ST
  in a level takes much longer than the others (straggler effect).
- Requires a barrier between levels — a slightly more complex coordinator.
- A level with many STs may exceed `max_worker_processes`.

#### Effort estimate

16–24 hours (builds on Option A).

---

### Option C — Async Refresh via `dblink`

#### Concept

Instead of spawning PostgreSQL background workers, the scheduler opens
additional database connections via `dblink` and fires refresh SQL
asynchronously, polling for completion.

```
scheduler ──┬── dblink_send_query(conn_a, 'SELECT pgtrickle.refresh_stream_table(...)')
            │   dblink_send_query(conn_b, 'SELECT pgtrickle.refresh_stream_table(...)')
            │
            └── dblink_get_result(conn_a)  ← poll
                dblink_get_result(conn_b)
```

#### Pros

- No `max_worker_processes` slots consumed — uses regular backend
  connections instead.
- Simpler implementation — no pgrx dynamic-worker ceremony.
- Each connection runs in its own transaction, providing isolation.

#### Cons

- **Requires `dblink` extension** — not always available (e.g., cloud PG
  providers may restrict it).
- Each connection consumes a backend slot (counts against
  `max_connections`).
- Connection string management — needs authentication credentials or a
  `dbname` parameter. On Unix-domain sockets this is simpler, but
  TCP setups require credential handling.
- Less control over error classification and retry — errors arrive as
  text from `dblink_error_message()`.
- Mixes SQL-level orchestration with the Rust scheduler, making the code
  harder to reason about.

#### Effort estimate

8–12 hours.

#### When to prefer

This option is attractive as a quick win if dynamic background workers
prove too complex, or if the deployment already uses `dblink` for other
purposes.

---

### Option D — External Orchestrator (Sidecar Process)

#### Concept

Move scheduling entirely outside PostgreSQL. A separate process (Rust
binary, Python script, or Kubernetes CronJob) maintains a connection pool
and calls `pgtrickle.refresh_stream_table(schema, name)` on multiple
connections in parallel. The in-database scheduler is disabled
(`pg_trickle.enabled = false`).

```
┌─────────────────────────────────────────┐
│         External orchestrator            │
│                                          │
│  conn_pool = Pool::new(max_connections) │
│                                          │
│  for level in dag_levels:                │
│      futures = []                        │
│      for st in level:                    │
│          futures.push(                   │
│              conn_pool.execute(           │
│                  refresh_stream_table()  │
│              )                           │
│          )                               │
│      await all(futures)                  │
│                                          │
└─────────────────────────────────────────┘
              │          │          │
         ┌────▼──┐  ┌───▼───┐  ┌──▼────┐
         │ PG    │  │ PG    │  │ PG    │    (backend connections)
         │ conn1 │  │ conn2 │  │ conn3 │
         └───────┘  └───────┘  └───────┘
```

#### Pros

- Unlimited parallelism, bounded only by the connection pool size.
- Easy integration with external monitoring (Prometheus, Kubernetes
  probes, Grafana alerts).
- No `max_worker_processes` pressure.
- Can be deployed as a Kubernetes sidecar alongside CNPG clusters.
- Language-independent — could be a Rust binary, Python script, or even
  a shell script with `psql`.

#### Cons

- **Operational complexity** — a separate process to deploy, monitor, and
  upgrade. Loses the "zero-config extension" appeal.
- Needs its own DAG awareness. Options:
  - Query `pgtrickle.pgt_stream_tables` + `pgtrickle.pgt_dependencies` to
    reconstruct the DAG externally.
  - Expose a SQL function `pgtrickle.get_refresh_order()` that returns
    levels.
- Connection pool sizing and authentication management.
- Manual intervention for crash recovery (or needs its own health-check
  loop).

#### Effort estimate

20–40 hours for a production-quality tool (Rust binary with DAG awareness,
connection pooling, retry logic, and health endpoints).

#### When to prefer

Best for large-scale deployments (100+ STs) where the single-worker
scheduler is a bottleneck and the operations team already manages sidecar
processes.

---

### Option E — Intra-Refresh Parallelism via PostgreSQL Parallel Query

#### Concept

No pg_trickle code changes needed. PostgreSQL's parallel query engine can
parallelize the **individual SQL statements** within a refresh — the delta
CTE query and the MERGE. This is orthogonal to inter-ST parallelism but
can dramatically speed up each individual refresh.

#### How it works

When the planner estimates a query will benefit from parallel execution
(large sequential scan, hash join, aggregation), it spawns parallel
workers that divide the work and merge results. The delta SQL and MERGE
statements are standard SQL and eligible for this optimization.

#### Relevant PostgreSQL GUCs

| GUC | Default | Purpose |
|---|---|---|
| `max_parallel_workers_per_gather` | `2` | Max parallel workers per Gather node |
| `max_parallel_workers` | `8` | Server-wide cap on parallel query workers |
| `parallel_setup_cost` | `1000` | Planner cost estimate for launching workers |
| `parallel_tuple_cost` | `0.1` | Per-tuple communication cost for parallel |
| `min_parallel_table_scan_size` | `8 MB` | Min table size for parallel seq scan |
| `min_parallel_index_scan_size` | `512 kB` | Min index size for parallel index scan |

#### Pros

- **Zero implementation effort** — already works today.
- Significant speedup for large table scans and aggregations in delta SQL.
- Composes with any of Options A–D (parallel query within each worker).

#### Cons

- Only parallelizes a single query at a time — does not help with inter-ST
  parallelism.
- Planner may choose not to parallelize small tables or complex CTEs.
- Parallel workers consume `max_worker_processes` slots temporarily.
- `pg_trickle.merge_planner_hints` already sets `SET LOCAL enable_nestloop
  = off` and raises `work_mem`, which may interact with parallel plan
  choices.

#### Verification

Run `EXPLAIN (ANALYZE)` on a delta SQL to confirm parallel plans are being
chosen:

```sql
EXPLAIN (ANALYZE, BUFFERS)
WITH __pgt_scan_o_1 AS (
    SELECT ...
    FROM pgtrickle_changes.changes_12345 c
    ...
)
SELECT * FROM __pgt_scan_o_1;
```

Look for `Gather` or `Gather Merge` nodes in the plan output.

---

## 3. Comparison Matrix

| | A: Dynamic BGW | B: Level-Parallel | C: dblink | D: External | E: Parallel Query |
|---|---|---|---|---|---|
| **Inter-ST parallelism** | Yes | Yes (optimal) | Yes | Yes | No |
| **Intra-refresh parallelism** | No* | No* | No* | No* | Yes |
| **Code complexity** | Medium | Medium-High | Low-Medium | High | None |
| **Effort (hours)** | 12–16 | 16–24 | 8–12 | 20–40 | 0 |
| **`max_worker_processes` pressure** | Yes | Yes | No | No | Yes (minor) |
| **`max_connections` pressure** | No | No | Yes | Yes | No |
| **Extension dependency** | None | None | `dblink` | None | None |
| **Operational overhead** | None | None | Low | High | None |
| **DAG ordering preserved** | Manual | Built-in | Manual | Manual | N/A |

\* Options A–D compose with E — parallel query can be active inside each
worker/connection.

---

## 4. Recommended Roadmap

### Phase 1 — Verify parallel query (0 hours, immediate)

Confirm that `max_parallel_workers_per_gather > 0` in production. Run
`EXPLAIN ANALYZE` on a representative delta SQL to verify that PostgreSQL
chooses parallel plans for large refreshes. Document findings. This is
**Option E** and costs nothing.

### Phase 2 — Level extraction in DAG (2–4 hours)

Add `StDag::topological_levels()` returning `Vec<Vec<NodeId>>`. This is a
prerequisite for Options A and B, is a small self-contained change, and has
value even without parallel dispatch (for monitoring — "which STs are in
which level?"). Wire the scheduler to iterate by level (still sequential
within each level, but now the levels are explicit).

### Phase 3 — Dynamic background workers (12–16 hours)

Implement **Option A + B** together: for each level, spawn up to
`max_concurrent_refreshes` dynamic workers, wait for them to complete, then
advance to the next level. This makes `max_concurrent_refreshes` fully
operational.

### Phase 4 (optional) — External orchestrator for large-scale

If Phase 3 proves insufficient for deployments with 100+ STs, build a
lightweight Rust sidecar (Option D) that reads the DAG from the catalog and
dispatches refreshes via a connection pool. This is an additive, opt-in
component that doesn't replace the in-database scheduler.

---

## 5. Open Questions

1. **Retry state storage** — Dynamic workers can't share the scheduler's
   in-memory `HashMap<i64, RetryState>`. Should retry state move to the
   catalog (`pgt_stream_tables.retry_attempts`, `retry_backoff_until`) or
   to shared memory?

2. **Worker PID tracking** — How should the coordinator detect worker
   completion? Options: (a) poll `pg_stat_activity` for the worker PID;
   (b) use a shared-memory slot array; (c) rely on
   `WaitForBackgroundWorkerShutdown`.

3. **Connection to database** — The current scheduler hard-codes
   `connect_worker_to_spi(Some("postgres"), None)`. Dynamic workers would
   need the same database. Multi-database support is out of scope.

4. **Straggler mitigation** — If one ST in a level takes 30 s and the
   rest take 200 ms, the entire level is blocked. Should the scheduler
   advance to the next level for STs whose dependencies are already
   satisfied (greedy dispatch)?

5. **Interaction with `pg_trickle.merge_planner_hints`** — Each dynamic
   worker calls `SET LOCAL` which is scoped to its own transaction. No
   cross-worker interference expected, but should be verified.
