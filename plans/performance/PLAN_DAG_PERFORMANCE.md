# DAG Topology & Refresh Propagation Performance

> **Status:** Analysis  
> **Date:** 2026-03-25 (updated for ST-to-ST differential, v0.11.0)  
> **Related:** [ARCHITECTURE.md](../../docs/ARCHITECTURE.md) ·
> [CONFIGURATION.md](../../docs/CONFIGURATION.md) ·
> [REPORT_PARALLELIZATION.md](REPORT_PARALLELIZATION.md) ·
> [STATUS_PERFORMANCE.md](STATUS_PERFORMANCE.md)

---

## 1. Overview

This report analyzes how DAG topology affects end-to-end change propagation
latency in pg_trickle — the time from a row being written to the most upstream
source table until it is visible in the most downstream stream table. The
analysis covers three refresh modes (sequential CALCULATED, scheduled, and
parallel), five representative topologies, and configuration tuning guidance.

### Key Variables

| Symbol | Meaning | Default |
|--------|---------|---------|
| $N$ | Total number of stream tables | — |
| $D$ | DAG depth (longest path from source to leaf) | — |
| $W$ | Maximum DAG width (STs at the widest level) | — |
| $T_r$ | Per-ST refresh wall-clock time | 5–200 ms |
| $I_s$ | Scheduler interval (`scheduler_interval_ms`) | 1000 ms |
| $I_p$ | Parallel poll interval (when workers in-flight) | 200 ms |
| $C$ | `max_concurrent_refreshes` (per-DB) | 4 |
| $C_g$ | `max_dynamic_refresh_workers` (cluster-wide) | 4 |

---

## 2. Refresh Modes

### 2.1 Sequential CALCULATED (no explicit schedule)

When stream tables have no explicit `SCHEDULE`, the scheduler uses upstream
change detection as the refresh trigger. The topological iteration order
guarantees upstream STs are refreshed before downstream STs **within the same
tick**. A key property of the implementation:

> *"CALCULATED STs: refresh whenever upstream sources have pending changes. The
> topological iteration order guarantees upstream STs are refreshed first within
> a tick, so their change buffers are already populated by the time we check
> here."*  
> — `src/scheduler.rs`, `check_schedule()`

After refreshing ST₁, the scheduler updates `data_timestamp` via SPI. When
ST₂ is evaluated next (same tick, same transaction),
`has_stream_table_source_changes()` compares the downstream's last-consumed
frontier position against the upstream ST's change buffer
(`changes_pgt_<pgt_id>`) and sees pending deltas from the refresh earlier in
the same tick. The change cascades through the **entire DAG in a single
tick**, regardless of depth.

**Propagation latency:**

$$L_{\text{calc}} = I_s + N \times T_r$$

The scheduler interval is the wait for the tick to begin; once inside the
tick, all $N$ STs are refreshed sequentially in topological order.

Since v0.11.0, ST-to-ST dependencies use dedicated change buffers
(`changes_pgt_<pgt_id>`), so downstream STs refresh **differentially** —
only the rows that changed in the upstream ST are propagated. Even when the
upstream ST performs a FULL refresh, a pre/post snapshot diff captures the
minimal INSERT/DELETE delta into the ST buffer, preventing FULL from
cascading through the chain. This means $T_r$ for downstream STs in an
ST-to-ST chain is proportional to the upstream delta size, not the full
table size.

### 2.2 Scheduled Mode (explicit `SCHEDULE`)

With explicit schedules (e.g., `SCHEDULE '1s'`), `check_schedule()` compares
`last_refresh_at` against the schedule interval. A downstream ST's schedule
timer is independent of its upstream's refresh — it won't fire until its own
interval expires. Each tick advances **at most one depth level** of the DAG.

**Propagation latency:**

$$L_{\text{sched}} = D \times \max(I_s, S)$$

where $S$ is the schedule interval. Since the scheduler only wakes every
$I_s$ milliseconds, the floor is the scheduler interval even if the schedule
is shorter (e.g., `SCHEDULE '100ms'` still propagates at $I_s = 1\text{s}$
per hop).

### 2.3 Parallel Mode (`parallel_refresh_mode = 'on'`)

The `ExecutionUnitDag` identifies independent execution units that can run
concurrently. The coordinator dispatches up to $C$ workers per database,
capped at $C_g$ cluster-wide. Each worker is a separate PostgreSQL background
worker process with its own SPI connection — spawned on demand, not pooled.

Between dispatch rounds, the scheduler sleeps for $I_p = 200\text{ms}$ (the
short poll interval used when workers are in-flight). Workers run as separate
background processes **concurrently** with this latch wait — $T_r$ is
absorbed into the poll period when $T_r \leq I_p$. When $T_r > I_p$, the
coordinator polls multiple times per batch; per-batch time ≈
$\lceil T_r / I_p \rceil \times I_p$.

Each round dispatches workers for all ready execution units (those whose
upstream dependencies have completed in the same tick via Step 1→Step 3 of
the dispatch loop).

**Propagation latency for a single level of width $W_l$:**

$$L_{\text{level}} = \left\lceil \frac{W_l}{C_{\text{eff}}} \right\rceil \times \max(I_p, T_r)$$

where $C_{\text{eff}} = \min(C, C_g - \text{workers\_used\_by\_other\_dbs})$.

**Total propagation latency:**

$$L_{\text{parallel}} = \sum_{l=1}^{D} \left\lceil \frac{W_l}{C_{\text{eff}}} \right\rceil \times \max(I_p, T_r)$$

For the common case $T_r \leq I_p = 200\text{ms}$, this simplifies to
$B \times I_p$ where $B = \sum_l \lceil W_l / C_{\text{eff}} \rceil$ is the
total number of dispatch rounds.

---

## 3. Topology Analysis

### 3.1 Linear Chain (depth = N, width = 1)

```
source → ST₁ → ST₂ → ST₃ → ... → ST_N
```

The worst case for propagation depth. Every ST depends on the previous one —
zero parallelism is possible.

Since v0.11.0, each hop is differential (O(delta), not O(table)), so $T_r$ at
each level depends on the upstream delta size rather than the full table size.
For a linear chain where 10 rows change at the base, each hop processes ~10
rows — $T_r$ stays constant (~5–20 ms) regardless of table size.

| Mode | Latency (N=500) |
|------|-----------------|
| CALCULATED | $1\text{s} + 500 \times T_r$ |
| SCHEDULE '1s' | $500 \times 1\text{s} = 8.3\text{min}$ |
| Parallel | $500 \times I_p \approx 100\text{s}$ — **worse than sequential** |

**Recommendation:** Use CALCULATED mode (sequential). Parallel mode adds
~200 ms of poll overhead per hop with no concurrency benefit, turning a
~5-second job into a ~100-second crawl. Do not enable `parallel_refresh_mode`
for linear chains.

### 3.2 Wide DAG (depth = D, width ≈ N/D)

```
source → [ST₁ .. ST₅₀] → [ST₅₁ .. ST₁₀₀] → ... → [ST₄₅₁ .. ST₅₀₀]
         level 1           level 2                   level 10
```

N=500, D=10, ~50 STs per level. Siblings at the same level are independent.

| Mode | Latency |
|------|---------|
| CALCULATED | $1\text{s} + 500 \times T_r$ (same as linear — all 500 run sequentially) |
| SCHEDULE '1s' | $10 \times 1\text{s} = 10\text{s}$ |
| Parallel (C=4) | $10 \times 13 \times I_p \approx 26\text{s}$ |
| Parallel (C=16) | $10 \times 4 \times I_p \approx 8\text{s}$ |

**At $T_r = 100\text{ms}$:**

| Mode | Latency |
|------|---------|
| CALCULATED | $\approx 51\text{s}$ |
| Parallel (C=16) | $10 \times 4 \times 200\text{ms} \approx 8\text{s}$ |

**Recommendation:** Parallel mode with $C = 16$ delivers significant speedup
over sequential for expensive refreshes. The cross-over point is where
CALCULATED cost ($I_s + N \times T_r$) equals parallel cost ($B \times I_p$);
for N=500, D=10, C=16 this is $T_r \approx 15\text{ms}$. For cheaper
refreshes, the 200 ms poll overhead dominates and sequential CALCULATED mode
is faster.

### 3.3 Fan-Out Tree (depth = D, exponentially widening)

```
source → ST₁ → [ST₂, ST₃] → [ST₄, ST₅, ST₆, ST₇] → ...
```

Common when a single source table feeds a master ST that branches into
domain-specific materialized views at each level.

| Level | STs at Level | Cumulative |
|-------|-------------|------------|
| 1 | 1 | 1 |
| 2 | 2 | 3 |
| 3 | 4 | 7 |
| ... | $2^{l-1}$ | $2^l - 1$ |

**Characteristics:**
- Early levels are bottlenecked by single-ST dependencies (parallel mode has
  little impact).
- Deeper levels benefit strongly from parallelism as width grows exponentially.
- CALCULATED mode handles the entire tree in one tick regardless.

**Recommendation:** Parallel mode helps significantly at the leaves. Set $C$
to match the expected leaf-level width, but keep in mind that the narrow upper
levels cannot benefit. A fan-out tree of depth 9 and 511 STs has 256 STs at
the leaf level — $C = 16..32$ is practical.

### 3.4 Diamond / Convergence Pattern

```
source → ST_A → ST_C
source → ST_B ↗
```

Multiple upstream STs feed into a shared downstream ST. pg_trickle groups
diamond patterns into **consistency groups** and (when
`diamond_consistency = 'atomic'`) refreshes all members within a single
sub-transaction to prevent inconsistent intermediate states.

**Consistency implications:**
- Atomic diamond groups add SAVEPOINT overhead (~1–2 ms per group).
- All members of the group must be refreshed together — the refresh time is
  the sum of all members, not the max.
- In parallel mode, atomic groups are dispatched as a single execution unit — 
  no intra-group parallelism.

**Recommendation:** Diamonds with many members (e.g., 10+ STs in one
consistency group) should be checked for necessity — often a simpler query
structure eliminates the diamond. When diamonds are unavoidable, ensure the
per-ST refresh time is low to keep the atomic group refresh fast.

### 3.5 Mixed Topology (Real-World)

Real deployments typically combine all of the above:

```
orders ──→ orders_daily ──→ orders_summary
        ├→ orders_by_region ─┤
        └→ orders_by_product ─┘──→ exec_dashboard
products ──→ product_stats ──────→ exec_dashboard
```

**Characteristics:**
- Heterogeneous depth: some paths are 2 hops, others are 4+.
- Diamond convergence at aggregation points (e.g., `exec_dashboard`).
- Mix of cheap STs (simple filters, ~5 ms) and expensive STs (multi-join
  aggregates, ~200 ms).

**Recommendation:** Use tiered scheduling (`pg_trickle.tiered_scheduling = true`)
to assign fast-changing operational STs to the `hot` tier (1× schedule) and
slow-changing analytical STs to `warm` (2×) or `economy` (10×). This reduces
wasted work without a complex DAG restructuring.

### 3.6 Cyclic SCCs (circular dependencies)

When `pg_trickle.allow_circular = true`, strongly connected components are
handled via **fixed-point iteration** — the scheduler refreshes all SCC
members repeatedly until convergence (zero net row-count delta) or the maximum
iteration limit (default 10).

**Propagation latency:**

$$L_{\text{scc}} = K \times |SCC| \times T_r$$

where $K$ is the number of iterations to convergence (typically 2–4 for
well-structured circular dependencies, up to 10 for pathological cases).

**Recommendation:** Circular dependencies are expensive. Each iteration
performs FULL refreshes of all SCC members. Keep SCCs small (2–3 members) and
ensure they converge quickly. Use `pgtrickle.refresh_stream_table()` with
`max_iterations` to test convergence offline before enabling in production.

---

## 4. Configuration Tuning Guide

### 4.1 `max_concurrent_refreshes` (per-database)

| DAG Shape | Recommended Value | Rationale |
|-----------|-------------------|-----------|
| Linear chain | N/A (`parallel_refresh_mode = 'off'`) | No parallelism possible |
| Width ≤ 10 | 8 | Clears each level in 1–2 batches |
| Width ~50 | **16** | Clears each level in ~4 batches |
| Width ~100+ | 24–32 | Diminishing returns beyond level width |

### 4.2 `max_dynamic_refresh_workers` (cluster-wide)

| Deployment | Recommended Value | Rationale |
|------------|-------------------|-----------|
| Single database | Match `max_concurrent_refreshes` | No contention from other DBs |
| 2–3 databases | 1.5× per-DB cap (e.g., 24) | Allow fair sharing |
| Many databases | 32–64 | Prevent starvation; per-DB caps divide the budget |

### 4.3 `max_worker_processes` (PostgreSQL)

Each dynamic refresh worker consumes one `max_worker_processes` slot. Budget:

$$\texttt{max\_worker\_processes} \geq 1_{\text{launcher}} + N_{\text{db}} + C_g + \text{autovac} + \text{parallel\_query} + \text{other}$$

| Target $C_g$ | Minimum `max_worker_processes` |
|---------------|-------------------------------|
| 4 (default) | 16 |
| 8 | 20 |
| 16 | 26 |
| 32 | 42 |

### 4.4 `scheduler_interval_ms`

The scheduler interval is the **floor** for single-hop propagation latency in
scheduled mode. Reducing it below 1000 ms increases tick frequency but also
increases CPU overhead from DAG evaluation, schedule checks, and SPI queries.

| Setting | Use Case |
|---------|----------|
| 1000 ms (default) | General purpose |
| 500 ms | Low-latency requirements with < 100 STs |
| 200 ms | Sub-second propagation for small DAGs (< 20 STs) |

**Warning:** Below 200 ms, the scheduler's own overhead (DAG rebuild check,
per-ST schedule evaluation, frontier comparison) may consume a significant
fraction of the tick, especially with 100+ STs.

### 4.5 Poll Interval & Dispatch Overhead

In parallel mode, the scheduler polls for worker completion every 200 ms
(`min(scheduler_interval_ms, 200)`). Workers run concurrently during this
wait. The "wasted wait" — fraction of the poll period spent idle after workers
have already completed — is:

$$\text{wasted wait} = \frac{I_p - T_r}{I_p} \quad (T_r \leq I_p)$$

When $T_r > I_p$, workers outlast one poll cycle; overhead becomes
$(\lceil T_r / I_p \rceil \times I_p - T_r)\,/\,(\lceil T_r / I_p \rceil \times I_p)$.

| $T_r$ | Wasted Wait | Assessment |
|--------|-------------|------------|
| 10 ms | 95% | Poll overhead dominates — sequential is faster at this scale |
| 50 ms | 75% | Marginal benefit; near break-even for N=500 workloads |
| 100 ms | 50% | Parallelism pays off for wide DAGs |
| 200 ms | ~0% | Maximum efficiency — workers fill the entire poll window |
| 300 ms+ | ~33% | $T_r > I_p$ regime; coordinator takes two polls per batch |

**Rule of thumb:** Enable parallel mode when $N \times T_r$ (sequential cost)
substantially exceeds $B \times I_p$ (parallel cost), where
$B = \sum \lceil W_l / C \rceil$ is the total dispatch rounds. For N=500,
D=10, C=16 the threshold is $T_r \approx 15\text{ms}$. The DAG must also
have meaningful width (≥ 4 independent STs per level) to benefit.

---

## 5. Performance Projections

### 5.1 Linear Chain (N=500, D=500, W=1)

| Mode | $T_r = 10\text{ms}$ | $T_r = 100\text{ms}$ |
|------|---------------------|----------------------|
| CALCULATED | **6 s** | **51 s** |
| SCHEDULE '1s' | 500 s (8.3 min) | 500 s (8.3 min) |
| Parallel (C=4) | ~100 s | ~100 s |
| Parallel (C=16) | ~100 s | ~100 s |

### 5.2 Wide DAG (N=500, D=10, W≈50)

| Mode | $T_r = 10\text{ms}$ | $T_r = 100\text{ms}$ |
|------|---------------------|----------------------|
| CALCULATED | **6 s** | **51 s** |
| SCHEDULE '1s' | 10 s | 10 s |
| Parallel (C=4) | ~26 s | ~26 s |
| Parallel (C=16) | ~8 s | ~8 s |
| Parallel (C=32) | ~4 s | ~4 s |

### 5.3 Fan-Out Tree (N=511, D=9, binary)

| Mode | $T_r = 10\text{ms}$ | $T_r = 100\text{ms}$ |
|------|---------------------|----------------------|
| CALCULATED | **6.1 s** | **52 s** |
| Parallel (C=16) | ~7 s | ~7 s |

The narrow upper levels (1–4 STs) are sequential bottlenecks; the wide lower
levels (64–256 STs) benefit from parallelism.

### 5.4 Summary: Optimal Mode by Topology

| Topology | Best Mode | Why |
|----------|-----------|-----|
| Linear chain | CALCULATED (sequential) | Zero parallelism; poll overhead hurts |
| Wide DAG, cheap refresh | CALCULATED | Poll overhead outweighs parallelism |
| Wide DAG, expensive refresh | **Parallel, C=16** | 4× speedup at the widest levels |
| Fan-out tree | Parallel or CALCULATED | Depends on leaf-level refresh cost |
| Small DAG (< 20 STs) | CALCULATED | Not worth the complexity |
| Mixed production | Parallel + tiered scheduling | Balance latency vs. resource usage |

> **Note (v0.11.0+):** All topologies above now benefit from differential
> ST-to-ST refresh. The per-hop $T_r$ for ST-sourced STs is proportional to the
> upstream delta size, not the full table size. This makes deep chains and
> fan-out trees significantly cheaper than the pre-v0.11.0 analysis assumed.

---

## 6. Key Architectural Insights

1. **CALCULATED mode cascades the full DAG in one tick.** Because stream-table-
   sourced STs use `data_timestamp` comparison (not CDC buffers) and the
   scheduler processes STs in topological order within a single transaction,
   the entire DAG converges in one pass. Depth is irrelevant for latency —
   only total ST count matters.

2. **Scheduled mode makes depth the bottleneck.** Each depth level requires a
   separate tick to propagate. Reducing DAG depth (by restructuring queries to
   reference source tables directly rather than intermediate STs) is the most
   effective optimization.

3. **Parallel mode trades per-hop overhead for width throughput.** The 200 ms
   poll interval creates a fixed cost per dispatch round. This is profitable
   only when the per-ST refresh time is comparable to or exceeds the poll
   interval, and when there are enough independent STs to fill the worker
   budget.

4. **Stream-table sources refresh differentially (v0.11.0+).** Since v0.11.0,
   upstream stream tables write their delta (INSERT/DELETE pairs) into
   dedicated ST change buffers (`changes_pgt_<pgt_id>`). The downstream ST's
   DVM scan operator reads from this buffer just like it reads from a
   base-table change buffer — enabling the full differential refresh path.
   When the upstream ST performs a FULL refresh, a pre/post `EXCEPT ALL`
   diff is captured into the ST buffer, so downstream STs still receive a
   minimal delta and never cascade to FULL. Frontier tracking uses
   `pgt_<upstream_pgt_id>` keys in the JSONB frontier to record the
   last-consumed position. This eliminates the previous limitation where
   ST-to-ST chains forced FULL refresh at each hop.

5. **Worker processes are ephemeral.** Each parallel refresh worker is a
   separate OS process with its own SPI connection — spawned, used, and
   terminated. There is no connection pool. High values of
   `max_concurrent_refreshes` increase process churn and connection setup
   overhead. The practical ceiling is ~32 for most deployments.

6. **Diamond consistency groups serialize their members.** Atomic diamond
   groups wrap all member refreshes in a sub-transaction. The group's refresh
   time is the sum of its members, not the max. In parallel mode, the entire
   group is dispatched as one unit — it occupies one worker slot but takes
   longer than a singleton unit.

---

## 7. Recommendations

### For low-latency propagation (< 5s end-to-end)

- Keep DAG depth ≤ 5.
- Use CALCULATED mode (no explicit schedule).
- Ensure event-driven wake is enabled (`pg_trickle.event_driven_wake = true`)
  for sub-50ms trigger-to-tick latency.
- ST-to-ST chains are now fully differential (v0.11.0+) — depth adds
  per-hop delta cost (proportional to changes) rather than full-table cost.
  Chains of 5–10 hops are practical when per-hop delta is small.

### For high-throughput wide DAGs (100+ STs)

- Enable `parallel_refresh_mode = 'on'`.
- Set `max_concurrent_refreshes` to half the widest DAG level, capped at
  available CPU cores.
- Set `max_dynamic_refresh_workers` equal to `max_concurrent_refreshes` for
  single-database deployments.
- Ensure `max_worker_processes` has sufficient headroom.

### For mixed production workloads

- Enable tiered scheduling to reduce unnecessary refresh cycles on
  low-priority STs.
- Use explicit `SCHEDULE` only for STs that require guaranteed freshness
  bounds — leave others as CALCULATED for fastest propagation.
- Monitor per-ST refresh duration via `pgtrickle.pgt_stream_tables` and
  tune the parallelism budget based on observed $T_r$ values.

### When to restructure the DAG

- **Linear chains > 20 deep:** With differential ST-to-ST (v0.11.0+), each
  hop costs O(delta) rather than O(table). Chains of 10–15 hops are now
  practical. Consider flattening only if per-hop overhead accumulates
  significantly (e.g., complex multi-join aggregates at each level).
- **Diamonds with 5+ members:** Simplify the query structure to reduce the
  consistency group size, or accept eventual consistency
  (`diamond_consistency = 'eventual'`) to allow independent refresh.
- **Circular dependencies:** Keep SCCs to 2–3 members maximum. Each fixpoint
  iteration is $|SCC| \times T_r$ and convergence is not guaranteed to be fast.

---

## 8. Further Improvement Opportunities

The following are potential optimizations that could further improve ST-to-ST
and general DAG refresh performance. Items are ordered by estimated
impact-to-effort ratio.

### 8.1 Intra-Tick Pipelining (High Impact)

**Problem:** In CALCULATED mode, the entire DAG is processed sequentially
within a single tick. Level $l+1$ cannot start until **all** of level $l$ is
complete, even when some level $l+1$ STs only depend on one completed ST
from level $l$.

**Proposal:** Within a single tick, begin processing an ST as soon as all its
upstream dependencies have completed — not when the entire previous level is
done. This is effectively **intra-tick parallelism with fine-grained
dependency tracking**.

**Expected gain:** For DAGs with mixed-cost levels (e.g., level 2 has one
expensive 500 ms ST and nine cheap 5 ms STs), level 3 STs that depend only
on cheap level 2 STs could start 495 ms earlier. For a depth-10 DAG with
heterogeneous costs, this could reduce end-to-end latency by 30–50%.

**Complexity:** Medium. Requires the parallel dispatch loop to track per-ST
completion (not per-level) and immediately enqueue newly-ready STs.

### 8.2 Adaptive Poll Interval (Medium Impact)

**Problem:** The 200 ms poll interval in parallel mode is a fixed constant.
For STs that complete in 5–10 ms, 95% of each poll period is wasted idle
time. For STs that take 300+ ms, the coordinator polls unnecessarily during
the first 200 ms.

**Proposal:** Use adaptive polling — start with a short poll (e.g., 20 ms),
double on each idle poll (exponential backoff to 200 ms), and reset to the
short poll when a worker completes. Alternatively, use `WaitLatch` with
worker completion signaling via shared memory flags.

**Expected gain:** For cheap refreshes ($T_r \approx 10\text{ms}$), this
could reduce the effective poll interval from 200 ms to ~20 ms, making
parallel mode competitive with CALCULATED even for DAGs with moderate width.

**Complexity:** Low–Medium. The latch-based approach is cleaner but requires
shared memory coordination; the adaptive poll is simpler to implement.

### 8.3 ST Buffer Compaction / Pruning (Medium Impact)

**Problem:** When a deep ST chain propagates a large batch, each level writes
the full delta to its ST change buffer (`changes_pgt_<pgt_id>`) and the next
level reads it. For a 10-level chain with a 10,000-row delta, this creates
10 × 10,000 = 100,000 rows of intermediate buffer data that is immediately
consumed and cleaned up.

**Proposal:** For single-consumer ST buffers (the common case), skip the
buffer write entirely — pass the delta directly in-memory to the downstream
ST within the same tick. This eliminates SPI overhead for the buffer INSERT
and DELETE cycle.

**Expected gain:** For deep chains with large deltas, eliminates 2 × SPI
DML operations per hop. At ~1 ms per DML for 10K rows, this saves ~20 ms
per hop — significant for chains deeper than 5 levels.

**Complexity:** High. Requires changing the DVM scan operator to optionally
read from an in-memory delta rather than a buffer table. Only applicable
when the downstream refresh happens within the same tick (CALCULATED mode).

### 8.4 Delta Amplification Detection (Medium Impact)

**Problem:** In multi-join ST chains, a small base-table delta can amplify
at each hop. For example, 1 changed row in a dimension table that joins with
a 1M-row fact table produces a 1M-row delta at the join level. If this ST
feeds another join, the delta amplifies further.

**Proposal:** Track delta size at each hop (already partially done via
`pgt_refresh_history`). When delta amplification exceeds a configurable
threshold (e.g., output delta > 100× input delta), emit a performance
warning and optionally fall back to FULL refresh for that specific hop
(which may be cheaper when the delta has exploded).

**Expected gain:** Prevents pathological cases where differential refresh is
actually slower than FULL due to delta amplification. The `explain_st()`
function could expose amplification metrics.

**Complexity:** Low. Most of the infrastructure exists — `pgt_refresh_history`
already tracks delta sizes. Just needs threshold comparison and a fallback.

### 8.5 Batch Coalescing for ST Buffers (Low–Medium Impact)

**Problem:** When event-driven wake triggers rapid-fire refreshes (low
`wake_debounce_ms`), an upstream ST may be refreshed 3 times before the
downstream reads its buffer. Each upstream refresh writes separate rows to
the ST buffer. The downstream then processes all 3 batches in one read.

**Proposal:** Coalesce matching INSERT/DELETE pairs within the ST buffer
before the downstream reads it. A net-effect pass (similar to
`compute_net_effect()` for base-table buffers) could cancel out INSERTs
followed by DELETEs for the same `__pgt_row_id`, reducing the delta size.

**Expected gain:** For rapidly-updating upstream STs, reduces downstream
delta size by eliminating intermediate states. Most impactful for
high-frequency OLTP workloads with many small updates to the same rows.

**Complexity:** Medium. The net-effect computation exists for base-table
buffers but needs adaptation for the ST buffer schema (no `old_*` columns).

### 8.6 Parallel ST-to-ST Within Atomic Groups (Low Impact)

**Problem:** Diamond consistency groups serialize all member refreshes into
a single sub-transaction on one worker. A group of 8 STs with $T_r = 50\text{ms}$
each takes 400 ms sequentially.

**Proposal:** Allow intra-group parallelism for members that don't share
sources — e.g., in a group `{A, B, C}` where A and B are independent but
C depends on both, A and B could run concurrently.

**Expected gain:** Limited to large diamond groups (5+ members) with
independent subsets — a niche but painful case.

**Complexity:** High. Requires sub-transaction coordination across workers,
which conflicts with the single-connection-per-worker model.

### Summary: Improvement Priority Matrix

| Improvement | Impact | Effort | Priority |
|-------------|--------|--------|----------|
| 8.1 Intra-tick pipelining | High | Medium | **P1** |
| 8.2 Adaptive poll interval | Medium | Low–Medium | **P2** |
| 8.4 Delta amplification detection | Medium | Low | **P2** |
| 8.3 ST buffer compaction | Medium | High | P3 |
| 8.5 Batch coalescing for ST buffers | Low–Medium | Medium | P3 |
| 8.6 Parallel within atomic groups | Low | High | P4 |
