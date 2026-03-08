# Plan: True Parallel Refresh Within a Database

Date: 2026-03-08
Status: Proposed
Last Updated: 2026-03-08

Related:
- [REPORT_PARALLELIZATION.md](../performance/REPORT_PARALLELIZATION.md)
- [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md)
- [PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md](PLAN_CROSS_SOURCE_SNAPSHOT_CONSISTENCY.md)
- [ARCHITECTURE.md](../../docs/ARCHITECTURE.md)

---

## 1. Problem Statement

pg_trickle currently runs one scheduler per database, and that scheduler
refreshes stream tables inline and sequentially. This is simple and correct,
but it leaves substantial throughput on the table when a database contains:

- many independent stream tables,
- wide DAG layers with no inter-table dependencies,
- a mix of small and large refreshes where one long refresh blocks many short
  ones, or
- tenant-like subgraphs that are independent but share a database.

The current implementation already has the beginnings of a parallel refresh
surface:

- `pg_trickle.max_concurrent_refreshes` exists as a GUC,
- advisory locks already prevent same-stream-table overlap,
- the scheduler already computes dependency-aware consistency groups, and
- the launcher already manages a two-tier worker model.

However, there is no true inter-stream-table parallelism today. A single heavy
refresh can delay the entire database's refresh cycle.

The goal of this plan is to add **true OS-level parallelism** within one
database while preserving correctness for:

- DAG dependency ordering,
- atomic diamond-consistency groups,
- IMMEDIATE downstream trigger behavior,
- retry/backoff semantics,
- manual refresh collision handling, and
- multi-database deployments that share the same PostgreSQL worker budget.

---

## 2. Design Decision

### 2.1 Chosen Model

Adopt a **single coordinator + many short-lived refresh workers** model:

- Keep **one scheduler coordinator per database**.
- Add **dynamic refresh workers** that execute refresh work in parallel.
- Treat the coordinator as the only authority for:
  - DAG rebuilds,
  - due-table selection,
  - dependency tracking,
  - retry state,
  - dispatch order, and
  - cluster-budget enforcement.

This is the right shape for “multiple workers per database.”

### 2.2 Explicit Non-Decision

Do **not** run multiple peer schedulers against the same database.

That would require distributed ownership of:

- the in-memory DAG,
- retry state,
- schedule evaluation,
- consistency-group policy,
- downstream readiness tracking, and
- crash recovery.

Multiple peer schedulers would turn a single scheduling problem into a
distributed coordination problem inside PostgreSQL. That is the wrong tradeoff.

### 2.3 Dispatch Model

Use a **ready-queue scheduler**, not a simple level barrier.

Topological levels are useful for analysis and observability, but a strict
“run level N, wait, then run level N+1” barrier leaves performance on the
table. Instead:

1. Build an execution-unit DAG.
2. Initialize a ready queue with all due units whose upstream prerequisites are
   satisfied.
3. Dispatch ready units while worker tokens are available.
4. As workers complete, decrement downstream dependency counts.
5. Immediately enqueue newly-ready units.

This preserves correctness while avoiding straggler-induced stalls.

---

## 3. Goals and Non-Goals

### Goals

1. Deliver true inter-stream-table parallelism inside one database.
2. Preserve all existing correctness guarantees for dependencies and atomic
   groups.
3. Keep manual refresh and scheduler refresh interoperable via advisory locks.
4. Respect PostgreSQL's cluster-wide `max_worker_processes` budget.
5. Allow safe incremental rollout behind a feature flag.
6. Reuse the existing scheduler architecture as much as possible.

### Non-Goals

1. Do not implement cross-worker distributed transactions.
2. Do not parallelize inside an atomic consistency group in v1.
3. Do not parallelize inside an IMMEDIATE-trigger closure in v1.
4. Do not replace the in-database scheduler with `dblink` or an external
   orchestrator.
5. Do not rely on PostgreSQL parallel query as the primary solution.
   Parallel query remains an orthogonal optimization inside each worker.

---

## 4. Core Correctness Model

Parallel execution must operate on **execution units**, not directly on raw
stream tables.

### 4.1 Execution Unit Types

An execution unit is one schedulable piece of work for a worker.

#### Unit A — Singleton stream table

The normal case: one stream table with no special grouping constraints.

#### Unit B — Atomic consistency group

If a consistency group requires `diamond_consistency = 'atomic'`, the entire
group becomes one execution unit. It is executed by one worker, serially,
inside the same worker transaction using the existing rollback semantics.

This means atomic groups remain **parallelizable with other independent units**,
but not internally parallelized.

#### Unit C — IMMEDIATE closure

IMMEDIATE mode introduces synchronous downstream trigger execution inside the
refresh transaction of an upstream stream table. This makes some apparently
independent scheduler nodes unsafe to run in parallel.

Example:

```
source -> A (DIFFERENTIAL) -> B (IMMEDIATE) -> C (DIFFERENTIAL)
```

When A refreshes, B updates inside the same transaction. B therefore cannot be
treated as an independent asynchronously schedulable node.

For v1, collapse any scheduler-visible subgraph that interacts through
IMMEDIATE-mode propagation into a single execution unit unless it is proven
safe to split.

This is intentionally conservative. It gives correctness first and parallelism
second.

### 4.2 Execution Unit DAG

The coordinator will no longer schedule directly on the raw `StDag`.
Instead it will build a second graph:

`StDag -> ExecutionUnitDag`

Transformation steps:

1. Start from the current stream-table DAG.
2. Compute existing consistency groups.
3. Collapse all atomic groups into super-nodes.
4. Detect IMMEDIATE-trigger closures and collapse them into super-nodes.
5. Add edges between resulting units.
6. Validate acyclicity again after collapsing.

The scheduler then runs only execution units.

### 4.3 Why IMMEDIATE Needs Special Handling

The current engine allows a scheduler-driven refresh to synchronously fire
statement-level IVM triggers on downstream IMMEDIATE tables. If two workers were
allowed to independently touch upstream nodes that converge into the same
IMMEDIATE table, they could concurrently drive writes into that IMMEDIATE table
from separate transactions.

Until that interaction is proven safe, the plan must treat IMMEDIATE-connected
components as serialization boundaries.

---

## 5. Chosen Architecture

### 5.1 Coordinator Responsibilities

The per-database scheduler remains a long-lived background worker and becomes a
pure coordinator. It will:

1. Rebuild the execution-unit DAG when catalog state changes.
2. Evaluate schedule due-ness and change presence.
3. Maintain in-memory retry state per execution unit.
4. Enqueue jobs for ready execution units.
5. Acquire a cluster worker token before spawning a dynamic refresh worker.
6. Track job completion and classify outcomes.
7. Release worker tokens and advance downstream readiness.
8. Run non-refresh coordinator-only tasks:
   - WAL transition advancement,
   - slot health checks,
   - stale alert emission,
   - retry-state pruning.

### 5.2 Refresh Worker Responsibilities

Each dynamic refresh worker will:

1. connect to the target database,
2. claim a specific job,
3. execute one execution unit,
4. write success/failure outcome metadata,
5. release its worker token, and
6. exit.

Workers are intentionally short-lived in v1. If worker spawn overhead becomes
measurable, a persistent worker-pool design can be a later optimization.

### 5.3 Job Protocol

Use a **catalog-backed job table** for dispatch and completion tracking,
combined with **shared-memory worker tokens** for cluster-wide capacity.

This split is deliberate:

- shared memory is ideal for fast cluster-global counters,
- a catalog table is easier for variable-length job payloads,
- job rows survive scheduler crashes and simplify recovery,
- composite units can store arrays of member `pgt_id`s without awkward fixed
  shared-memory sizing.

---

## 6. New State and Metadata

### 6.1 New Catalog Table: `pgtrickle.pgt_scheduler_jobs`

Add a new internal table for execution-unit dispatch.

Proposed columns:

| Column | Type | Purpose |
|---|---|---|
| `job_id` | `bigserial` | Unique job id |
| `dag_version` | `bigint` | Execution graph generation used when enqueued |
| `unit_key` | `text` | Stable identifier for one execution unit |
| `unit_kind` | `text` | `singleton`, `atomic_group`, `immediate_closure` |
| `member_pgt_ids` | `bigint[]` | Stream tables contained in the unit |
| `root_pgt_id` | `bigint` | Primary ST for singleton-like units |
| `status` | `text` | `QUEUED`, `RUNNING`, `SUCCEEDED`, `RETRYABLE_FAILED`, `PERMANENT_FAILED`, `CANCELLED` |
| `scheduler_pid` | `int` | Coordinator that enqueued the job |
| `worker_pid` | `int` | Worker that claimed the job |
| `attempt_no` | `int` | Retry attempt count |
| `enqueued_at` | `timestamptz` | Queue timestamp |
| `started_at` | `timestamptz` | Worker start |
| `finished_at` | `timestamptz` | Worker finish |
| `outcome_detail` | `text` | Error or skip context |
| `retryable` | `bool` | Worker-classified retryability |

Indexes:

- `(status, enqueued_at)` for polling active jobs
- `(unit_key, status)` to avoid duplicate in-flight jobs
- `(finished_at)` for cleanup

Retention policy:

- keep active jobs always,
- keep recent completed jobs for debugging,
- periodically prune older completed rows.

### 6.2 New Shared Memory State

Extend shared memory with a cluster-global dynamic worker budget.

Proposed fields:

```rust
struct PgTrickleParallelState {
    max_dynamic_refresh_workers: u32,
    active_dynamic_refresh_workers: u32,
    last_reconcile_epoch: u64,
}
```

This state is cluster-global, not per database.

It exists to prevent multiple database coordinators from overcommitting the
shared PostgreSQL worker budget.

### 6.3 New GUCs

#### `pg_trickle.parallel_refresh_mode`

Enum:

- `off` — current behavior
- `dry_run` — compute units and log dispatch decisions, but execute inline
- `on` — enable dynamic refresh workers

Default: `off` initially.

#### `pg_trickle.max_dynamic_refresh_workers`

Cluster-wide cap on concurrently active pg_trickle refresh workers.

This is distinct from `pg_trickle.max_concurrent_refreshes`, which remains the
**per-database** dispatch cap.

#### Reuse `pg_trickle.max_concurrent_refreshes`

Reinterpret the existing GUC as:

> maximum active refresh workers per database coordinator

This makes the existing setting finally meaningful.

---

## 7. Scheduling Algorithm

### 7.1 Readiness Tracking

For each execution unit in the current DAG generation, the coordinator keeps:

- `remaining_upstreams`,
- `due_this_cycle`,
- `inflight`,
- `last_outcome`, and
- retry/backoff state.

Only units with:

- `remaining_upstreams == 0`,
- `due_this_cycle == true`,
- `inflight == false`, and
- `not in backoff`

enter the ready queue.

### 7.2 Queue Ordering

Within the ready queue, order by:

1. earliest freshness deadline,
2. then schedule urgency,
3. then topological position as a tie-breaker.

This preserves the existing scheduler's bias toward due/overdue work.

### 7.3 Dispatch Loop

Pseudocode:

```text
build execution_unit_dag
seed ready_queue

while ready_queue not empty or inflight_jobs not empty:
    while ready_queue not empty
      and per_db_active < max_concurrent_refreshes
      and cluster_worker_token available:
        pop next unit
        create job row
        spawn refresh worker(job_id)
        mark inflight

    poll active jobs

    for each completed job:
        update retry state
        release token
        clear inflight
        if success:
            decrement downstream remaining_upstreams
            enqueue newly ready units
        if failure:
            block dependent units for this cycle if prerequisites unmet

run coordinator-only background tasks
```

### 7.4 Failure Semantics

If a unit fails:

- its direct downstream units are not eligible this cycle,
- unrelated branches continue,
- retry/backoff remains coordinator-owned,
- atomic-group failures remain all-or-nothing inside the worker.

This is more efficient than today's “everything waits behind one failing node”
effect while preserving dependency correctness.

---

## 8. Worker Execution Semantics

### 8.1 Singleton Unit

The worker runs the existing refresh path for one stream table. This should
reuse as much of `execute_scheduled_refresh()` as possible.

### 8.2 Atomic Group Unit

The worker executes all members serially in one transaction using the existing
sub-transaction rollback pattern already implemented for atomic consistency
groups.

The important change is only **where** the work runs, not **how** the group is
made atomic.

### 8.3 IMMEDIATE Closure Unit

The worker executes the root scheduled refresh. Any downstream IMMEDIATE work
that fires synchronously inside the transaction remains inside the same worker.

The coordinator must not independently schedule any member of that closure.

### 8.4 Advisory Locks Remain Authoritative

Workers still acquire per-stream-table advisory locks before refresh.

This keeps compatibility with:

- manual `refresh_stream_table()` calls,
- concurrent scheduler workers across databases,
- any future operational tooling that triggers refreshes directly.

If a worker cannot obtain a lock for any required member:

- singleton units become `RETRYABLE_FAILED`,
- atomic units roll back and become `RETRYABLE_FAILED` as a whole.

---

## 9. Crash Recovery and Reconciliation

Parallel dispatch introduces new failure modes. Recovery must be explicit.

### 9.1 On Coordinator Startup

The coordinator should:

1. scan `pgt_scheduler_jobs` for rows in `RUNNING` or `QUEUED`,
2. check whether the owning worker PID still exists,
3. mark orphaned jobs as failed or cancelled,
4. reconcile the shared-memory worker-token count against reality, and
5. rebuild in-memory retry and readiness state from the current catalog.

### 9.2 On Worker Startup

The worker should verify:

1. the job row still exists,
2. the job belongs to its database,
3. the DAG generation has not become obsolete if that matters for safe
   execution,
4. the coordinator PID is still alive, or that executing the job is still safe
   even if the coordinator exited.

If validation fails, the worker should mark the job cancelled and exit.

### 9.3 Token Reconciliation

Never trust only the in-memory counter after a crash.

Add a reconciliation path that recomputes the active count from live
`pg_stat_activity` entries with application name `pg_trickle refresh worker`.

This avoids leaked capacity after abnormal exits.

---

## 10. Implementation Phases

### Phase 0 — Instrumentation and Safety Rails

#### Scope

- Add new GUCs.
- Add unit-graph building in memory.
- Add `dry_run` mode.
- Add logging that shows which units would run in parallel.

#### Files

- `src/config.rs`
- `src/dag.rs`
- `src/scheduler.rs`
- `docs/ARCHITECTURE.md`
- `docs/CONFIGURATION.md`

#### Task List

- Add `pg_trickle.parallel_refresh_mode` with `off`, `dry_run`, and `on`.
- Add `pg_trickle.max_dynamic_refresh_workers` as a cluster-wide cap.
- Wire scheduler logging to emit the computed execution units and queue order.
- Keep execution inline in `dry_run` mode while recording would-dispatch events.
- Add tests proving `dry_run` produces the same refresh outcomes as current
  serial execution.

#### Acceptance Criteria

- Scheduler can log execution units and ready-queue order without changing
  behavior.
- `dry_run` mode shows the same serial refresh results as today.

### Phase 1 — Execution Unit DAG

#### Scope

- Introduce `ExecutionUnit` and `ExecutionUnitDag` types.
- Collapse atomic consistency groups into units.
- Add conservative IMMEDIATE-closure collapsing.
- Revalidate acyclicity after collapsing.

#### Files

- `src/dag.rs`
- `src/scheduler.rs`
- `src/ivm.rs` or relevant metadata helpers if needed

#### Task List

- Add `ExecutionUnitId`, `ExecutionUnit`, and `ExecutionUnitDag` data types.
- Convert current consistency groups into atomic execution units.
- Detect IMMEDIATE-connected closures and collapse them conservatively.
- Build dependency edges between units and compute unit-level topological order.
- Add unit tests for singleton, diamond, IMMEDIATE, and mixed graphs.

#### Acceptance Criteria

- Unit DAG matches raw DAG for plain singleton cases.
- Atomic diamonds appear as one unit.
- IMMEDIATE-connected unsafe components are not split across units.

### Phase 2 — Job Table and Worker Budget

#### Scope

- Add `pgt_scheduler_jobs` migration.
- Add catalog CRUD helpers.
- Add shared-memory token pool for refresh workers.
- Add startup reconciliation.

#### Files

- `sql/pg_trickle--<prev>--<next>.sql`
- `src/catalog.rs`
- `src/shmem.rs`
- `src/lib.rs`
- `src/config.rs`

#### Task List

- Create `pgtrickle.pgt_scheduler_jobs` and indexes.
- Add Rust catalog helpers for enqueue, claim, complete, cancel, and prune.
- Extend shared memory with cluster-wide refresh-worker counters.
- Add reconciliation logic using both job rows and live `pg_stat_activity`.
- Add tests for orphaned jobs and leaked-token cleanup.

#### Acceptance Criteria

- Coordinators can create and poll job rows.
- Worker budget is enforced cluster-wide.
- Crash-restart reconciliation restores token correctness.

### Phase 3 — Dynamic Worker Entry Point

#### Scope

- Add `pg_trickle_refresh_worker_main`.
- Pass `job_id` via dynamic-worker startup.
- Implement singleton execution-unit handling first.
- Persist worker outcome to the job row.

#### Files

- `src/scheduler.rs`
- `src/refresh.rs`
- `src/catalog.rs`

#### Task List

- Add a dedicated refresh-worker entry point and application name.
- Implement safe job claim/verify logic at worker startup.
- Reuse current singleton refresh execution inside the worker.
- Persist success, retryable failure, and permanent failure to the job table.
- Add integration tests proving worker execution matches current inline behavior.

#### Acceptance Criteria

- One singleton unit can be executed by a dynamic worker with the same result
  as the current inline scheduler path.
- Retryable vs permanent failure classification is preserved.

### Phase 4 — Coordinator Dispatch Loop

#### Scope

- Replace inline singleton refresh with ready-queue dispatch.
- Keep coordinator-owned retry state.
- Release downstream units immediately on prerequisite completion.
- Keep fallback path: if no worker token is available, optionally defer or run
  inline depending on feature mode.

#### Files

- `src/scheduler.rs`
- `src/error.rs`
- `src/monitor.rs`

#### Task List

- Add in-memory ready queue and in-flight job tracking.
- Dispatch singleton units while respecting per-db and cluster-wide limits.
- Update downstream readiness immediately on worker completion.
- Preserve existing retry/backoff semantics at coordinator level.
- Add tests showing overlap of independent units and isolation of failing
  branches.

#### Acceptance Criteria

- Independent singleton units overlap in time.
- Downstream units begin as soon as prerequisites complete.
- Unrelated work continues even when one branch is failing.

### Phase 5 — Composite Units

#### Scope

- Run atomic consistency groups inside refresh workers.
- Run IMMEDIATE-closure units inside refresh workers.
- Ensure the coordinator never double-schedules unit members.

#### Files

- `src/scheduler.rs`
- `src/dag.rs`
- `src/ivm.rs`
- `src/refresh.rs`

#### Task List

- Move existing atomic-group execution into worker-owned unit execution.
- Add composite-unit dispatch payloads with member `pgt_id` arrays.
- Make coordinator scheduling aware of collapsed-unit membership.
- Add IMMEDIATE-closure execution path that preserves same-transaction effects.
- Add E2E tests for atomic diamonds and IMMEDIATE chains under parallel mode.

#### Acceptance Criteria

- Atomic groups retain all-or-nothing behavior.
- IMMEDIATE downstream propagation remains in one worker transaction.
- No member of a composite unit is scheduled separately.

### Phase 6 — Observability and Tuning

#### Scope

- Add monitoring for active refresh workers, queue depth, token exhaustion,
  job latency, and blocked-ready units.
- Expose coordinator vs worker health in monitoring functions.
- Update documentation and operational guidance.

#### Files

- `src/monitor.rs`
- `docs/SQL_REFERENCE.md`
- `docs/FAQ.md`
- `docs/ARCHITECTURE.md`
- `docs/CONFIGURATION.md`

#### Task List

- Add metrics or status functions for active workers and queued jobs.
- Surface token exhaustion and blocked-ready counts to operators.
- Document worker-budget sizing and failure-recovery behavior.
- Extend health checks so they distinguish coordinator death from saturation.
- Add operational examples for multi-database deployments.

#### Acceptance Criteria

- Operators can explain why work is waiting.
- Monitoring distinguishes “scheduler alive but saturated” from “scheduler not
  running.”

### Phase 7 — Rollout and Default Change

#### Scope

- Keep `parallel_refresh_mode = off` by default until all tests are stable.
- Dogfood in CI behind explicit coverage.
- Consider later defaulting to `on` only after real-world validation.

#### Task List

- Keep feature gated behind `off/dry_run/on` through initial releases.
- Add CI coverage that runs selected suites with parallel mode enabled.
- Record benchmark deltas against serial mode before enabling by default.
- Update changelog and release notes when the feature graduates.
- Reassess default mode only after stability and operational evidence.

#### Acceptance Criteria

- Full test matrix passes with parallel mode enabled.
- Documentation clearly states worker-budget requirements.

---

## 11. Test Plan

### Unit Tests

1. Execution-unit DAG construction:
   - singleton DAG
   - atomic diamond collapse
   - IMMEDIATE closure collapse
   - mixed graph with independent and collapsed units
2. Ready-queue transitions and downstream release.
3. Retry-state transitions from worker outcomes.
4. Token accounting and reconciliation logic.

### Integration Tests

1. Two independent stream tables refresh concurrently.
2. A downstream table does not start until upstream success.
3. A failing branch does not block an unrelated branch.
4. Advisory-lock collision with manual refresh yields retryable worker failure.
5. Token exhaustion stops over-dispatch.
6. Orphaned job reconciliation after simulated worker crash.

### E2E Tests

1. Atomic diamond group still rolls back as one unit.
2. IMMEDIATE-in-the-middle chain remains correct under parallel scheduler mode.
3. Multi-database cluster sharing worker budget does not exceed configured
   limits.
4. Refresh-history timing shows true overlap for independent units.

### Benchmark / Performance Tests

Measure at least:

1. sequential vs parallel wall-clock refresh time for 10, 50, and 100
   independent stream tables,
2. scheduler throughput under mixed short and long refreshes,
3. worker-spawn overhead,
4. impact on `max_worker_processes` headroom.

---

## 12. Expected Performance Envelope

This plan should improve **refresh throughput** and **freshness latency** for
independent work inside one database. It should not be treated as a primary
solution for improving **source-table write throughput**.

### 12.1 What Should Improve

In the current scheduler, cycle time is approximately the sum of all due
refresh durations:

`T_serial ~= sum(t_i)`

With this plan, cycle time should move toward the larger of:

- the DAG critical-path time, and
- total due work divided by the effective worker count,

plus worker-spawn and coordination overhead.

In practical terms:

- Wide DAG layers with many independent singleton units should see the largest
   gains.
- Mixed workloads with a few long refreshes and many short refreshes should see
   better throughput than a strict level barrier because the ready queue can
   release downstream work as soon as prerequisites finish.
- Tenant-like independent subgraphs in one database should stop blocking each
   other behind one long-running refresh.

### 12.2 Best-Case Shape

The best case is:

- many independent singleton execution units,
- enough cluster worker headroom,
- little IMMEDIATE-induced collapsing, and
- little time spent in atomic consistency groups.

Example:

- 50 independent stream tables
- each refresh takes about 200 ms
- effective worker count of 4

Current serial cycle: about 10 s.

Expected parallel cycle after this plan: roughly 2.5-3.5 s, depending on spawn
overhead and scheduling gaps.

This is not a guarantee, but it is the right order of magnitude for a balanced,
parallelizable workload.

### 12.3 Moderate-Gain Shape

Moderate gains are more likely than ideal speedups in real deployments.

Expected outcome:

- total wall-clock cycle time decreases,
- short independent refreshes complete earlier,
- one failing or slow branch stops blocking unrelated branches, and
- `pg_trickle.max_concurrent_refreshes` starts affecting observed throughput.

This is the most realistic outcome for deployments with partially independent
DAGs and mixed refresh sizes.

### 12.4 Low-Gain or No-Gain Shape

This plan will provide little benefit when the workload is dominated by:

- one long linear dependency chain,
- large atomic consistency groups,
- IMMEDIATE-connected closures that must be collapsed into one unit,
- tiny refreshes where worker-spawn overhead dominates, or
- a saturated cluster worker budget.

In those cases the system remains correctness-limited or resource-limited, not
scheduler-limited.

### 12.5 What This Plan Does Not Fix

This plan does **not** materially raise the write-throughput ceiling of the
current trigger-based CDC path.

It should improve how quickly accumulated changes are processed once refreshes
run, but it does not remove per-row trigger overhead on source tables. If the
main problem is sustained DML throughput on tracked sources, that remains a CDC
architecture issue rather than a scheduler issue.

---

## 13. Acceptance Criteria

This plan is complete when all of the following are true:

1. Independent execution units in the same database can refresh concurrently in
   separate background workers.
2. The scheduler uses a ready queue, not only a flat topological list.
3. Atomic consistency groups remain all-or-nothing.
4. IMMEDIATE-trigger interactions are not incorrectly split across workers.
5. Cluster-wide refresh-worker budget is enforced across multiple databases.
6. Crash recovery reconciles stale jobs and leaked worker tokens.
7. Observability explains active workers, queued work, and blocked readiness.
8. `pg_trickle.max_concurrent_refreshes` has real effect in production.

---

## 14. Risks and Mitigations

| Risk | Why it matters | Mitigation |
|---|---|---|
| Worker-budget exhaustion | Parallel refresh workers compete with other PostgreSQL workers | Add explicit cluster-wide cap and document headroom requirements |
| IMMEDIATE interaction bugs | Scheduler-visible DAG may hide synchronous trigger side effects | Collapse IMMEDIATE closures conservatively in v1 |
| Scheduler crash while workers run | Can leak jobs or capacity | Catalog-backed jobs + token reconciliation on startup |
| Too much complexity in one step | Parallel scheduler changes touch core correctness logic | Ship behind `off/dry_run/on` rollout modes |
| Spawn overhead reduces benefit for tiny jobs | Short-lived workers are not free | Measure, then consider persistent worker pools later |

---

## 15. Follow-On Work After v1

Once this plan is stable, the next optimizations are:

1. persistent per-database refresh worker pools instead of short-lived workers,
2. less conservative splitting of IMMEDIATE closures where proven safe,
3. smarter work-stealing across ready units,
4. per-unit cost estimation for better queue ordering,
5. composition with PostgreSQL parallel query inside each worker.

The important first milestone is not “maximum possible parallelism.” It is
“correct, observable, bounded true parallelism.”