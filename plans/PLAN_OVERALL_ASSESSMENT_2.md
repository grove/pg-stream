# Overall Project Assessment — Deep Analysis Report (v0.23.0)

**Status:** Assessment report
**Type:** REPORT
**Last updated:** 2026-04-19
**Scope:** Repository-wide review of pg_trickle at **v0.23.0** (main, post v0.21/v0.22/v0.23 ship)
**Target audience:** Maintainers planning v0.24.0+, the v0.25.0 Relay CLI, and v1.0
**Sibling document:** [PLAN_OVERALL_ASSESSMENT.md](PLAN_OVERALL_ASSESSMENT.md) — the v0.20.0 baseline that this report supersedes

---

## Executive Summary

pg_trickle has matured rapidly between v0.20.0 (the baseline of the
prior assessment) and v0.23.0. The v0.21–v0.23 cycle landed a large
portion of the items called out in the previous report: `refresh.rs`
was split into a sub-module (ARCH-1), 28 parser unwraps were
eliminated (SAF-1), 42% of `unsafe` blocks were retired (SAF-2),
`#![deny(clippy::unwrap_used)]` is enforced in non-test code (SAF-3),
the recursive-CTE differential path (P2-1) is fully implemented, the
in-database parallel refresh worker pool exists, downstream CDC
publication is supported, and a predictive cost model + SLA-driven
tier auto-assignment shipped. The codebase as it stands at v0.23.0
is in noticeably better shape than three releases ago.

The **highest-impact remaining risks** identified by this fresh audit
are: (1) the EC-01 join-delta phantom-row bug is still **not fixed in
production code** despite being deferred multiple times — it is only
papered over via the Q15 IMMEDIATE-skip allowlist; (2) frontier
advancement and change-buffer truncation are not bracketed by a
two-phase commit, leaving a real (if narrow) window where a scheduler
crash between TRUNCATE and frontier-store can produce phantom replays
or silent data loss; (3) `pgt_refresh_history` and the L1
template-cache still grow without bound; (4) the catalog snapshot is
re-loaded into the scheduler on every tick, becoming the dominant
cost above ~200 stream tables; and (5) the new v0.21–v0.23 surface
(`metrics_server.rs`, `api/publication.rs`, the predictive model) has
**near-zero unit-test coverage** despite being on the critical path.

The most exciting forward-looking opportunities are: a **shared
shmem template cache (L0)** that would close the connection-pooler
latency gap; a **transactional outbox with at-least-once delivery
guarantees** (already on the roadmap as v0.24.0) that would let
pg_trickle act as the source of truth for downstream event-driven
systems; a **PGlite WASM build** (planned for v1.4) that would unlock
in-browser reactive dataflow; and **incremental DAG maintenance** to
make 1000-ST deployments routine.

---

## Table of Contents

1. [Method & Scope](#1-method--scope)
2. [What Has Improved Since v0.20.0](#2-what-has-improved-since-v0200)
3. [Critical Issues Found](#3-critical-issues-found)
4. [Architecture & Design Gaps](#4-architecture--design-gaps)
5. [Performance Bottlenecks & Optimization Opportunities](#5-performance-bottlenecks--optimization-opportunities)
6. [Test Coverage Gaps](#6-test-coverage-gaps)
7. [Recommended New Features (Exciting & High-Impact)](#7-recommended-new-features-exciting--high-impact)
8. [Roadmap Recommendations for Future Releases](#8-roadmap-recommendations-for-future-releases)
9. [Detailed Findings by Component](#9-detailed-findings-by-component)
10. [Risk Assessment](#10-risk-assessment)
11. [Conclusions & Priorities](#11-conclusions--priorities)

---

## 1. Method & Scope

**Inputs reviewed**

- [ROADMAP.md](../ROADMAP.md), [CHANGELOG.md](../CHANGELOG.md),
  [AGENTS.md](../AGENTS.md), [ESSENCE.md](../ESSENCE.md)
- All top-level source modules in [src/](../src/) (now reorganised
  with `src/refresh/`, `src/api/`, and `src/dvm/` subdirectories)
- The previous assessment in
  [PLAN_OVERALL_ASSESSMENT.md](PLAN_OVERALL_ASSESSMENT.md) (used as
  diff baseline; not duplicated)
- [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md),
  [docs/SQL_REFERENCE.md](../docs/SQL_REFERENCE.md),
  [docs/CONFIGURATION.md](../docs/CONFIGURATION.md),
  [docs/DVM_OPERATORS.md](../docs/DVM_OPERATORS.md),
  [docs/DVM_REWRITE_RULES.md](../docs/DVM_REWRITE_RULES.md)
- 134 integration test files in [tests/](../tests/)
- Plan documents under [plans/](.) and version implementation details in
  [roadmap/v0.21.0.md-full.md](../roadmap/v0.21.0.md-full.md),
  [roadmap/v0.22.0.md-full.md](../roadmap/v0.22.0.md-full.md),
  [roadmap/v0.23.0.md-full.md](../roadmap/v0.23.0.md-full.md)

**Out of scope**

- Running the full test suite (Docker-based E2E)
- Byte-level audit of every remaining `unsafe` block (~80 sites
  remain, all with `// SAFETY:` comments per SAF-2)
- TUI internals (pgtrickle-tui/), dbt-pgtrickle/ adapter internals
- Benchmark numbers (only architectural cost estimates are quoted)

**What changed in methodology vs. the v0.20 report**

This pass deliberately concentrated on regressions, new modules
introduced after v0.20.0, and forward-looking opportunities. Items
already addressed in v0.21–v0.23 are explicitly noted as resolved so
the maintainers can focus on the residue.

---

## 2. What Has Improved Since v0.20.0

The following findings from the prior assessment are now **resolved
or substantially mitigated** and are explicitly removed from the open
items list:

| Prior finding | Status | Evidence |
|---|---|---|
| 2.2 Parser `.unwrap()` panics (28 sites in `sublinks.rs`) | ✅ Fixed | SAF-1, v0.21.0 changelog |
| 2.3 547 unsafe blocks, no plan in flight | ✅ Reduced 42% (479→277) | SAF-2; safe façades introduced |
| 2.4 Monolithic `refresh.rs` (8.4k LOC) | ⚠️ Sub-module created | ARCH-1 — `src/refresh/{codegen,phd1,merge,orchestrator}.rs` exist as landing zones; **migration is incomplete** (most code still lives in `mod.rs`) |
| 2.6 `PLAN_NON_DETERMINISM` not started | ✅ Done | OP-6, ND1–ND4 in v0.21 |
| 3.1 No in-database parallel refresh | ✅ Shipped | v0.22 PAR-1..5; `pg_trickle.max_parallel_workers` GUC |
| 3.2 No downstream CDC emitter | ✅ Shipped | v0.22 `stream_table_to_publication()` |
| 3.6 Dog-feeding is reactive only | ✅ Predictive model added | v0.22 PRED-1..4 |
| 5.2 No `pause_all/resume_all` | ✅ Shipped | v0.21 |
| 5.3 No `refresh_if_stale` | ✅ Shipped | v0.21 |
| 5.4 No `stream_table_definition` | ✅ Shipped | v0.21 |
| 6.1 Three big files with no unit tests | ⚠️ Partial — TEST-1/2/3 added 75+ tests for `helpers.rs`, `diagnostics.rs`, `parser/rewrites.rs`; new modules below are now the gap |
| 6.2 No fuzz target | ✅ `parser_fuzz` added | TEST-4 |
| 6.3 No bgworker crash-recovery test | ✅ 3 tests added | TEST-5, `tests/resilience_tests.rs` |
| 8.3 No Prometheus exporter | ✅ Shipped | OP-2, `src/metrics_server.rs` |
| 8.4 No canary mode | ✅ Shipped | `canary_begin/diff/promote` in v0.21 |
| Recursive CTE DRed deferred | ✅ Implemented | `src/dvm/operators/recursive_cte.rs` — semi-naive + DRed + recomputation strategies |

The investments in v0.21–v0.23 paid off: the previously-flagged
"easy wins" are gone, and what remains is harder but better-scoped.

---

## 3. Critical Issues Found

### Issue: EC-01 join-delta phantom rows (still unresolved)

- **Severity:** **Critical** (silent wrong results)
- **Location:** [src/dvm/operators/join.rs](../src/dvm/operators/join.rs#L234-L245),
  [src/refresh/mod.rs](../src/refresh/mod.rs#L2363-L2380), L4991–L5005;
  IMMEDIATE_SKIP_ALLOWLIST in
  [tests/e2e_tpch_tests.rs](../tests/e2e_tpch_tests.rs#L91-L110)
- **Description:** When EC-01 splits a JOIN delta into parts 1a
  (Δ⋈R₁) and 1b (Δ⋈R₀), the `__pgt_row_id` hash continues to depend
  on the right-side PK. For a co-deleted right row, R₀ ≠ R₁, so the
  two halves emit *different* row ids for the same logical row, and
  weight aggregation cannot cancel them across cycles. PH-D1 only
  deletes the row ids present in the current delta, leaving phantoms
  from prior cycles in place. The Q15 stop-gap (EC01-0) was added in
  v0.21.0; the proper fix (EC01-1..4) was deferred to v0.22.0 in the
  CHANGELOG and **has still not landed** in v0.23.0.
- **Impact:** TPC-H Q07/Q15 produce extra rows under multi-table
  LEFT/RIGHT/FULL JOIN with mixed UPDATE/DELETE workloads. Any user
  ST built on the same shape silently drifts; aggregate queries can
  mask the problem by averaging away the phantom contribution.
- **Recommendation:** Promote EC01-1..4 to **v0.24.0 P0**. Either
  (a) hash *only* the left-side PK on Part 1b so the two halves
  converge on a single row id, or (b) recompute Part 1b from CDC
  pre-image columns. Add a 5,000-iteration proptest that asserts
  cross-cycle convergence under random INSERT/UPDATE/DELETE
  sequences. Until fixed, keep Q15 in `IMMEDIATE_SKIP_ALLOWLIST` and
  document the limitation prominently in
  [docs/PATTERNS.md](../docs/PATTERNS.md).

### Issue: Frontier and change buffer not committed atomically

- **Severity:** **High** (narrow data-loss / phantom-replay window on crash)
- **Location:** [src/refresh/mod.rs](../src/refresh/mod.rs#L2960-L3030)
  (TRUNCATE of change buffer);
  [src/scheduler.rs](../src/scheduler.rs#L5120-L5220) (frontier
  store);
  [src/catalog.rs](../src/catalog.rs#L550-L590)
  (`store_frontier_and_complete_refresh`)
- **Description:** The optimised path stores frontier + completion
  timestamp in a single SPI call, but it is not always taken (manual
  refresh and adaptive fallback follow a different code path that
  truncates the change buffer first and updates the frontier
  afterwards). Between the two operations there is no checkpoint.
  Buffers are also `UNLOGGED`, so a server crash means change rows
  written by triggers since the last refresh are lost.
- **Impact:** If the bgworker crashes between TRUNCATE and frontier
  store: next tick re-reads the (now-empty) buffer and *also* skips
  rows because the frontier never advanced. With UNLOGGED buffers,
  any uncommitted-by-pgtrickle delta is gone after `crash recovery`.
  This violates the project goal "data loss is unacceptable".
- **Recommendation:** Implement two-phase frontier commit: write a
  *tentative* frontier to a side column before TRUNCATE; finalise
  after MERGE commits; reconcile on startup. Optionally provide a
  GUC `pg_trickle.change_buffer_durability = unlogged|logged|sync`
  (default `unlogged` to preserve current performance) that lets
  durability-sensitive deployments opt into WAL-logged buffers. This
  is a tractable v0.24.0 feature.

### Issue: Two production `unwrap()` calls remain in CDC

- **Severity:** **High** (backend crash on parser-tree edge case)
- **Location:** [src/cdc.rs](../src/cdc.rs#L3145) — two
  `build_changed_cols_bitmask_expr(...).unwrap()` calls in the
  trigger-emission path
- **Description:** The SAF-1/SAF-3 sweep eliminated all
  `unwrap_used` violations via `#![deny]`, but two
  `unwrap_or_else(panic)`-equivalent sites remain in cdc.rs that the
  lint does not catch.
- **Impact:** A malformed parse tree (e.g., from a rarely-exercised
  PG 18 minor release diff) would `PANIC` the bgworker, which on
  PostgreSQL means a **postmaster restart of the entire server**.
- **Recommendation:** Convert both sites to `?` propagation with a
  new `PgTrickleError::ChangedColsBitmaskFailed` variant. Trivial,
  one-day task.

### Issue: Partitioned source table conversion silently freezes WAL CDC

- **Severity:** **High**
- **Location:** [src/wal_decoder.rs](../src/wal_decoder.rs#L65-L95)
- **Description:** When a base table is converted from non-partitioned
  to partitioned *after* a WAL publication has been created, the
  publication continues to advertise only the parent OID without
  `publish_via_partition_root = true`. The WAL decoder reads zero
  rows from child partitions and the stream table silently freezes.
- **Impact:** Stream tables go stale with no error visible to the
  user. This is a foot-gun for OLTP shops that adopt time-series
  partitioning after the fact.
- **Recommendation:** On scheduler tick, compare
  `pg_publication_tables.pubviaroot` against the current
  `relkind = 'p'` of each source. If mismatched, rebuild the
  publication and emit a `WARNING` in
  `pgt_refresh_history.refresh_reason = 'publication_rebuild'`.

### Issue: TOAST-only column updates are not detected

- **Severity:** **Medium-High**
- **Location:** [src/cdc.rs](../src/cdc.rs)
- **Description:** Trigger-based CDC writes both `old_*` and `new_*`
  for all columns, but UPDATEs that change a TOASTable column without
  triggering a TOAST-pointer rewrite (for instance, jsonb `||`
  in-place merges that PostgreSQL stores externally) can produce
  identical hashes for old and new versions. The differential refresh
  then misclassifies the row as unchanged.
- **Impact:** Silent data drift in stream tables that project or
  filter on TOASTed text/jsonb/array columns.
- **Recommendation:** Use `pg_column_size()` and (where available)
  the TOAST chunk identifier as part of the hash for TOASTable
  columns; or fall back to comparing detoasted values when a column
  flagged `attstorage IN ('e', 'x')` is referenced. Add a TOAST
  workload to `tests/e2e_cdc_edge_case_tests.rs`.

---

## 4. Architecture & Design Gaps

### Gap: ARCH-1 sub-module migration is structural-only

- **Area:** Architecture / Maintainability
- **Description:** v0.21 created `src/refresh/{codegen.rs,
  orchestrator.rs, phd1.rs, merge.rs}` as landing zones, but the
  bulk of the logic still lives in `src/refresh/mod.rs` (~8,200
  LOC). The submodules are stubs with comment headers.
- **Evidence:** [src/refresh/mod.rs](../src/refresh/mod.rs#L15-L30)
  — `mod codegen; mod merge; mod orchestrator; mod phd1;` — but
  each file is <50 LOC of placeholder.
- **Recommendation:** Schedule a v0.24.0 follow-up
  (ARCH-1B) that actually migrates the four concerns. Each move can
  be a small PR; the lint `clippy::unwrap_used` already prevents
  regression on safety. Without this, the original "monolith risk"
  finding from the prior assessment is unchanged in practice.

### Gap: Catalog snapshot reload on every scheduler tick

- **Area:** Scalability
- **Description:** `load_stream_tables()` runs full SPI SELECT against
  `pgtrickle.pgt_stream_tables` every scheduler tick (default 5 s).
  At 10 STs the cost is ~2 ms; at 1000 STs it is ~200 ms — a
  significant fraction of the tick budget and the dominant cost
  above 200 STs.
- **Evidence:** [src/scheduler.rs](../src/scheduler.rs#L340-L380)
- **Recommendation:** Cache the snapshot in shmem, invalidated by
  the existing `DAG_REBUILD_SIGNAL` atomic. Pair with
  [§5 Bottleneck #11](#5-performance-bottlenecks--optimization-opportunities)
  for a single-PR change. This is the highest-ROI scalability fix
  remaining in the codebase.

### Gap: `pgt_refresh_history` has no retention policy

- **Area:** Operability / Storage
- **Description:** History accumulates one row per refresh. At 100
  STs × 12 refreshes/min that is ~17M rows/month; for IMMEDIATE-mode
  deployments it is many times higher.
- **Evidence:** [src/catalog.rs](../src/catalog.rs#L1834-L1847) —
  insert path; no deleter outside the manual cleanup helper.
- **Recommendation:** Ship `pg_trickle.refresh_history_retention_days`
  (default 7) and a bgworker-driven prune that runs in 1k-row batches
  (the v0.20 deletion code already exists for the manual variant —
  reuse it). Document the GUC in
  [docs/CONFIGURATION.md](../docs/CONFIGURATION.md).

### Gap: L1 template cache is per-backend and unbounded

- **Area:** Memory / Pooler latency
- **Description:** [src/template_cache.rs](../src/template_cache.rs)
  is a thread-local `HashMap` with no eviction. A pooled connection
  that touches 1000 STs sequentially holds ~4 MB of cached SQL
  templates; a 100-connection pool wastes ~400 MB of duplicated
  state. The L2 catalog cache exists but each session pays a
  cold-start tax on first touch (~45 ms parse + differentiate).
- **Evidence:** [src/template_cache.rs](../src/template_cache.rs#L30-L50)
- **Recommendation:** Add an L0 dshash in shmem so all backends in
  the same database share one compiled template set. Bound the L1
  cache with an LRU policy (`pg_trickle.template_cache_max_entries`,
  default 256).

### Gap: Cluster-wide worker budget is shared across databases

- **Area:** Multi-tenancy / Fairness
- **Description:** `max_dynamic_refresh_workers` is a single GUC; per-DB
  quotas exist (`per_database_worker_quota`) but default to "first
  come first served". One busy database can starve others.
- **Evidence:** [src/scheduler.rs](../src/scheduler.rs#L1410-L1430)
- **Recommendation:** Add a `pgtrickle.worker_allocation_status()`
  monitoring view (per-DB used/quota/queued); ship a recommended
  default (e.g., quota = ceil(total / N_databases)) and document the
  pattern in [docs/SCALING.md](../docs/SCALING.md). Long-term: round-robin
  fair-share dispatch within the launcher.

### Gap: DAG cache invalidation is per-process, not lock-free

- **Area:** Concurrency / DDL latency
- **Description:** Every backend that calls into pg_trickle keeps a
  RefCell-cached topological order. Cross-process invalidation goes
  through `DAG_REBUILD_SIGNAL`, but the rebuild itself takes the
  exclusive `PGS_STATE` lock for the duration of an O(V+E) walk.
- **Evidence:** [src/dag.rs](../src/dag.rs#L540-L620),
  [src/shmem.rs](../src/shmem.rs#L190-L330)
- **Recommendation:** (a) Use copy-on-write so writers compute the
  new DAG out-of-line and atomically swap an `Arc`; (b) defer
  rebuilds to idle scheduler ticks where possible; (c) consider
  incremental update (add/remove an edge) for the common case.
  This becomes critical above 500 STs.

### Gap: Predictive cost model has no robustness guards

- **Area:** Correctness of automation
- **Description:** v0.22's linear regression
  ([src/api/publication.rs](../src/api/publication.rs)) is a
  one-line `fit_linear_regression` over the last *N* refreshes.
  There is no protection against (a) outliers (one 30-second spike
  poisons the slope for the next hour), (b) cold-start (predictions
  fire before n ≥ min_samples?), (c) zero-variance (constant delta
  size → division by zero), or (d) NaN/Inf propagation.
- **Recommendation:** Add unit-tested guards: clamp predictions to
  `[0.5×, 4×] last_full_ms`; use median+MAD instead of mean+SD;
  require `min_samples` *and* a non-degenerate variance; ignore
  predictions during the first 60 s after a CREATE.

### Gap: Multi-database isolation is per-process, not workload-aware

- **Area:** Multi-tenancy
- **Description:** Each database gets its own bgworker and shared-mem
  block, which is correct, but cross-database fairness, OOM
  attribution, and per-DB Prometheus labels are absent.
- **Recommendation:** Tag every Prometheus metric with `db_oid` (the
  endpoint serves a single DB today); expose
  `pgtrickle.cluster_worker_summary()` from the postmaster; document
  the recommended pattern in
  [docs/integrations/multi-tenant.md](../docs/integrations/) (new
  page).

### Gap: No "frozen stream table" detector

- **Area:** Observability / Reliability
- **Description:** The PT3 partitioned-source bug (above) is one
  example of a class of failures where an ST simply stops advancing
  with no error logged. There is no built-in monitor that says "ST
  X has not advanced its frontier in N intervals despite source
  activity".
- **Recommendation:** Add a self-monitoring view
  `df_frozen_stream_tables` that flags any ST whose
  `last_refresh_at < now() - 5 × refresh_interval` *and* whose source
  has had recent CDC writes. Alert via the existing
  `pgtrickle_alert` NOTIFY channel.

### Gap: Downstream publication has no end-to-end durability story

- **Area:** Distributed correctness
- **Description:** v0.22's `stream_table_to_publication()` exposes a
  PostgreSQL logical-replication publication, but pg_trickle does
  not own the slot lifecycle, does not advance LSN coordinated with
  refresh commits, and cannot detect a lagged or stuck subscriber.
- **Recommendation:** Track subscriber LSN per publication; refuse
  to TRUNCATE the change buffer until all subscribers have
  acknowledged past the buffer's max LSN; emit a `WARNING` when a
  subscriber lags more than `pg_trickle.publication_lag_warn_lsn`.

---

## 5. Performance Bottlenecks & Optimization Opportunities

### Bottleneck: Catalog snapshot reload per tick (HIGH ROI)

- **Location:** [src/scheduler.rs](../src/scheduler.rs#L340-L380)
- **Impact:** 20 ms / tick at 100 STs → 200 ms / tick at 1000 STs.
- **Cause:** Full SPI SELECT every 5 seconds.
- **Recommendation:** Cache in shmem keyed by DAG version; invalidate
  on DDL via the existing signal. Estimated win: 20–200 ms / tick.

### Bottleneck: Per-source change-detection issues N queries

- **Location:** [src/cdc.rs](../src/cdc.rs#L237-L275),
  [src/refresh/mod.rs](../src/refresh/mod.rs#L1662-L1669)
- **Impact:** 10 ms / tick at 10 sources; scales with source count.
- **Cause:** One SPI SELECT per change buffer; no batching even when
  all sources belong to the same ST.
- **Recommendation:** Combine into a single `UNION ALL` CTE per ST
  (or per refresh group). Estimated win: ~80% of current cost.

### Bottleneck: Missing indexes on internal catalog tables

- **Location:** [sql/](../sql/) (no `CREATE INDEX` for these
  predicates)
- **Impact:** 5–15 ms / tick at 100+ STs (sequential scans).
- **Cause:** The following columns are filtered without index
  coverage:
  - `pgt_stream_tables(status, scc_id)` — scheduler ready-set
  - `pgt_refresh_history(pgt_id, action, data_timestamp)` — monitoring
  - `pgt_change_tracking(source_relid)` — change ownership
  - `changes_<oid>(__pgt_action, __pgt_ts)` — partial-index candidate
- **Recommendation:** Add four `CREATE INDEX IF NOT EXISTS` statements
  in `sql/pg_trickle--0.23.0--0.24.0.sql`. Trivial change.

### Bottleneck: `pg_trickle_hash_multi` allocates per row

- **Location:** [src/hash.rs](../src/hash.rs#L30-L41)
- **Impact:** 100–200 ns × rows-per-second; significant at high
  ingest.
- **Cause:** String concatenation of column values before hashing.
- **Recommendation:** Switch to `xxh3` streaming (`update`/`finalize`)
  which avoids the intermediate buffer. Optionally reuse a
  per-transaction `String` arena.

### Bottleneck: Project operator emits per-column `format!`

- **Location:**
  [src/dvm/operators/project.rs](../src/dvm/operators/project.rs#L70-L90)
- **Impact:** 50–100 String allocations per refresh on wide STs.
- **Recommendation:** Use a single pre-sized `String` and `write!`.
  Cumulative win is small per refresh but compounds on
  `IMMEDIATE`-mode triggers.

### Bottleneck: PGS_STATE single LwLock taken exclusively

- **Location:** [src/shmem.rs](../src/shmem.rs#L61-L65)
- **Impact:** 100–500 ns per acquisition, taken on every tick on
  every DB.
- **Recommendation:** Split into `dag_lock`, `metrics_lock`,
  `worker_pool_lock`; use `share()` for read-only `dag_version`
  reads in the launcher.

### Bottleneck: Spawn cost per dynamic refresh worker

- **Location:** [src/scheduler.rs](../src/scheduler.rs#L190-L202)
- **Impact:** ~2 ms per registration × N workers/tick.
- **Recommendation:** Optional persistent worker pool
  (`pg_trickle.worker_pool_size` GUC, default 0 = current
  behaviour). Workers loop on a shmem queue instead of being
  spawned per-task.

### Opportunity: Adaptive cost-model state lives in catalog

- **Location:** [src/refresh/mod.rs](../src/refresh/mod.rs#L6037-L6050)
- **Cause:** `last_full_ms`/`last_diff_ms` read via SPI on each
  refresh; with parallel workers two refreshes can read stale data
  and both pick DIFFERENTIAL when FULL would be faster.
- **Recommendation:** Cache per-ST timing in shmem with atomic
  updates; invalidate on schedule.

### Opportunity: Parallel aggregate merge for large GROUP BY

- The Welford pipeline is single-threaded. For >1M groups, a
  parallel merge using `pg_parallel_safe`-marked aggregates is
  correctness-safe and roughly halves latency.

---

## 6. Test Coverage Gaps

### Gap: New v0.21–v0.23 modules have near-zero unit tests

| File | LOC | Unit tests | Public functions untested |
|---|---|---|---|
| [src/metrics_server.rs](../src/metrics_server.rs) | ~130 | 0 | `start`, `serve_one_request`, port-conflict & timeout paths |
| [src/api/publication.rs](../src/api/publication.rs) | ~530 | 2 | `stream_table_to_publication`, `set_stream_table_sla`, `fit_linear_regression`, `predict_diff_duration_ms`, `should_preempt_to_full`, `assign_tier_for_sla`, `maybe_adjust_tier_for_sla` |
| [src/api/diagnostics.rs](../src/api/diagnostics.rs) | 1,766 | ~4 | `explain_query_rewrite`, `diagnose_errors`, `list_auxiliary_columns`, `validate_query`, 5 `gather_*` helpers |

- **Risk:** The predictive cost model is now part of the automation
  loop; a bug there silently mode-switches user STs incorrectly.
- **Recommendation:** Carve a 50-test campaign across these three
  files (TEST-6/7/8 in v0.24.0). All can be pure-Rust.

### Gap: No crash-recovery test for downstream publication

- **Risk:** A subscriber crash mid-replay or a postmaster crash with
  an active `stream_table_to_publication()` was never tested.
- **Recommendation:** Extend `tests/resilience_tests.rs` with a
  "stop subscriber, kill postmaster, restart, verify catch-up"
  scenario.

### Gap: No predictive-cost-model accuracy harness

- **Recommendation:** Add `tests/e2e_predictive_cost_tests.rs` that
  drives a saw-tooth workload, a bursty workload, and a single-spike
  workload, asserting (a) the model recovers within N samples after
  an outlier, (b) tier assignments do not oscillate >2 times per
  hour, and (c) preemption to FULL only fires when actually faster.

### Gap: No "two parallel workers pick the same task" test

- **Risk:** The worker pool dispatcher relies on `FOR UPDATE SKIP
  LOCKED` semantics; a regression in dispatch logic (e.g., a stale
  cache) could allow duplicate execution.
- **Recommendation:** Add a deterministic E2E test that pre-registers
  a slow refresh under one worker, then asks the dispatcher for a
  second task and asserts it picks something different.

### Gap: No simultaneous `ALTER` + `REFRESH` integration test

- **Risk:** `alter_stream_table(query => ...)` rebuilds the catalog
  while another worker may be mid-refresh. Behaviour is currently
  undefined under contention.
- **Recommendation:** Add concurrent ALTER/REFRESH + concurrent
  DROP/REFRESH scenarios to `tests/e2e_concurrent_tests.rs`.

### Gap: No SLA tier oscillation test

- **Risk:** SLA = 10 s with actual latency oscillating around 10 s
  could thrash hot ↔ warm forever.
- **Recommendation:** Property test asserting at most 2 transitions
  per simulated hour for any oscillating workload.

### Gap: Fuzz coverage limited to parser

- **Recommendation:** Add fuzz targets for: cron-expression parser
  (DoS risk on pathological strings), GUC string→enum coercion, and
  CDC trigger payload deserialisation. All low-effort.

### Gap: No partition-count limit / scale-up test

- **Risk:** No test asserts behaviour at >100 partitions on a
  single source. Trigger-install cost is O(N) and untested.
- **Recommendation:** Add a `#[ignore]`-by-default scale test in
  `tests/e2e_partition_tests.rs` that creates 1,000 partitions and
  asserts wall-clock < 60 s.

### Gap: No TOAST workload in CDC edge-case suite

- **Recommendation:** Add jsonb-update and bytea-update scenarios to
  `tests/e2e_cdc_edge_case_tests.rs` that target the §3 Issue
  "TOAST-only updates not detected".

---

## 7. Recommended New Features (Exciting & High-Impact)

### Feature: Transactional outbox / inbox primitives (already on roadmap as v0.24.0)

- **Category:** Distributed correctness / Integration
- **Description:** First-class `OUTBOX` and `INBOX` table types with
  exactly-once semantics, automatic dead-lettering, and built-in
  retry. `pgtrickle.create_outbox(name, target_publication)`
  publishes change events with strong ordering guarantees;
  `pgtrickle.create_inbox(name, source_publication)` consumes from
  an upstream replication slot and applies idempotently.
- **User value:** Replaces hand-rolled outbox tables in microservice
  architectures; pg_trickle becomes the *transactional bridge*
  between OLTP and event-driven consumers (Kafka, NATS, Redis Streams).
- **Estimated effort:** Large (several weeks)
- **Priority:** **High** — already planned for v0.24.0; this report
  reinforces its prioritisation.
- **Design notes:** Build on top of the v0.22 publication primitive;
  reuse `pgt_refresh_history` for retry book-keeping; add a
  `pgt_outbox_offsets` catalog table tracking per-subscriber LSN.

### Feature: `pgtrickle-relay` CLI (already on roadmap as v0.25.0)

- **Category:** Ecosystem / Integration
- **Description:** A standalone CLI that bridges outbox→sinks
  (Kafka/NATS/HTTP) and sources→inbox (Webhook/Kafka/CDC).
  Effectively a "pg_trickle agent" that lives outside the database
  for use cases where the destination cannot accept logical
  replication directly.
- **User value:** Frees pg_trickle from a "you must use logical
  replication" assumption; opens HTTP webhooks, Kafka without
  Debezium, NATS, MQTT.
- **Priority:** High
- **Design notes:** Use `tokio` + `pgrx` shared crate for type
  fidelity; ship as a single binary; configuration via TOML.

### Feature: Shared shmem L0 template cache

- **Category:** Performance / Pooler latency
- **Description:** A `dshash` cache in shared memory that all
  backends in the same DB consult before parsing. Invalidated by
  the same generation counter the L1 cache already uses.
- **User value:** Eliminates the 30–45 ms cold-start tax on
  pooled-connection workloads (PgBouncer transaction mode, AWS RDS
  Proxy, Supabase pooler).
- **Estimated effort:** Medium (1–2 weeks)
- **Priority:** High

### Feature: Two-phase frontier commit + opt-in WAL-logged buffers

- **Category:** Correctness / Durability
- **Description:** New `pg_trickle.change_buffer_durability` GUC
  (`unlogged` / `logged` / `sync`). The `logged` and `sync` modes
  WAL-log change buffer rows; the two-phase commit (tentative
  frontier → MERGE → finalise) closes the crash-replay window.
- **User value:** Eliminates the silent-data-loss class of bugs;
  closes a real gap relative to the project goal "data loss is
  unacceptable".
- **Priority:** High

### Feature: Automatic EC-01 join correctness fix + pre-image capture

- **Category:** Correctness
- **Description:** Implement EC01-1..4 as planned. Capture
  pre-image columns in the CDC trigger; use them on the Part 1b
  arm so row-id hashes converge across cycles.
- **User value:** Removes the IMMEDIATE-skip allowlist; restores
  Q15 / Q07 correctness; lifts the "join + UPDATE/DELETE" foot-gun
  warning from `docs/PATTERNS.md`.
- **Priority:** **Critical**

### Feature: Stream-table snapshot / point-in-time restore

- **Category:** Operability / DR
- **Description:** `pgtrickle.snapshot_stream_table(name, target)`
  exports `(pgt_id, frontier, content_hash, rows)` to an archival
  table; `pgtrickle.restore_from_snapshot(name, source)` rehydrates
  on a fresh replica without re-running the full defining query.
- **User value:** New replicas come online in seconds rather than
  hours; PITR aligns ST state with logical wall-clock.
- **Priority:** Medium-High

### Feature: Predictive maintenance window planner

- **Category:** Operability / Self-driving
- **Description:** Use the v0.22 predictive cost model to recommend
  a refresh schedule (`pgtrickle.recommend_schedule(name)`). A
  longer-term variant could automatically request a maintenance
  window in advance of a forecast spike.
- **User value:** Reduces manual schedule-tuning toil.
- **Priority:** Medium

### Feature: Cross-DB / cross-cluster fan-out via `pgtrickle-relay`

- **Category:** Scale / Ecosystem
- **Description:** A relay-mode that reads from one cluster's
  outbox, applies to another cluster's inbox. Optional CRDT-style
  conflict resolution for last-write-wins counters.
- **User value:** Multi-region read-replica patterns without a full
  replication setup.
- **Priority:** Medium (post-v0.25.0)

### Feature: PGlite WASM build (already on roadmap as v1.4.0)

- **Category:** Ecosystem / Reactive UI
- **Description:** Compile pg_trickle to WASM for PGlite, exposing
  reactive stream tables in the browser.
- **User value:** Killer demo for offline-first apps; aligns with
  the v1.5 reactive-integration plan.
- **Priority:** High (long-term)

### Feature: GUI workflow designer + query lab

- **Category:** Usability
- **Description:** Extend `pgtrickle-tui` with a visual DAG editor
  and an EXPLAIN-DIFF preview that shows the rewritten DVM SQL
  alongside expected delta size.
- **User value:** Lowers the on-boarding ramp for SQL developers
  unfamiliar with IVM semantics.
- **Priority:** Medium

---

## 8. Roadmap Recommendations for Future Releases

### Near-term (v0.24.0)

- **Critical:** Implement EC-01 join phantom-row fix (EC01-1..4) and
  remove Q15 from `IMMEDIATE_SKIP_ALLOWLIST`.
- **High:** Two-phase frontier commit + `change_buffer_durability` GUC.
- **High:** Eliminate the two `unwrap()` sites in `cdc.rs`.
- **High:** Add `pg_trickle.refresh_history_retention_days` GUC and
  bgworker pruner.
- **High:** Cache catalog snapshot in shmem (10× scheduler win
  above 200 STs).
- **Medium:** Add 50+ unit tests across `metrics_server.rs`,
  `api/publication.rs`, and `api/diagnostics.rs` (TEST-6/7/8).
- **Medium:** Continue ARCH-1B refactor — actually move code from
  `src/refresh/mod.rs` into the four sub-modules.
- **Medium:** Detect partitioned-source publication mismatch and
  rebuild publication with `pubviaroot = true`.
- Already-planned: transactional inbox/outbox primitives.

### Mid-term (v0.25.0 – v0.27.0)

- **Ship `pgtrickle-relay`** (already v0.25.0). Add a webhook sink and
  a Kafka source as the first two adapters.
- **Shared shmem template cache** to close the connection-pooler
  latency gap.
- **TOAST-aware CDC hashing** + corresponding edge-case test suite.
- **Frozen-stream-table detector** in self-monitoring.
- **Subscriber-LSN tracking** for downstream publications;
  publication-lag warning.
- **Robustness guards on the predictive cost model** (median+MAD,
  outlier clamping, cold-start protection).
- **Incremental DAG maintenance** for >500 ST deployments.

### Long-term (6+ months → v1.0)

- **PGlite WASM** (already v1.4.0). Pair with reactive integration
  (v1.5.0) for the killer demo.
- **PostgreSQL 17 backport** (v1.1.0).
- **Core extraction** to `pg_trickle_core` (v1.3.0) — required for
  PGlite, useful for a future SQLite/DuckDB bridge.
- **Stream-table snapshot/restore** for fast replica bootstrap.
- **Cross-cluster fan-out** through the relay (multi-region patterns).
- **Visual workflow designer** in the TUI / web extension.

---

## 9. Detailed Findings by Component

### Component: `src/refresh/`

- **Current state:** Sub-module directory exists; ~95% of code
  still lives in `mod.rs`. Functionally correct but the original
  monolith risk is unmoved.
- **Issues:** ARCH-1 incomplete; frontier/buffer not committed
  atomically; PH-D1 leaves prior-cycle phantoms; manual-refresh
  path diverges from optimised path.
- **Recommendations:** Land ARCH-1B; unify the two refresh code
  paths; implement two-phase frontier commit.

### Component: `src/dvm/operators/`

- **Current state:** Excellent factoring; 21 operators, each with a
  shared test-helpers module. Recursive CTE differential is fully
  implemented (semi-naive, DRed, recomputation fallback).
- **Issues:** EC-01 join phantom rows (Critical, see §3).
- **Recommendations:** EC01-1..4 fix; proptest harness for
  cross-cycle convergence on JOINs.

### Component: `src/cdc.rs`, `src/wal_decoder.rs`

- **Current state:** Cleanly separated; per-table `cdc_mode`
  override implemented; auto-fallback hardened.
- **Issues:** Two production `unwrap()` calls; partitioned-table
  conversion silently freezes WAL CDC; TOAST-only updates missed;
  unlogged buffers risk crash data loss.
- **Recommendations:** Convert unwraps to typed errors; add
  publication-rebuild hook; TOAST-aware hashing; opt-in logged
  buffers.

### Component: `src/scheduler.rs`, `src/dag.rs`

- **Current state:** Parallel worker pool, SLA tier auto-assignment,
  predictive preemption, self-monitoring, and canary-mode all wired in.
  ~5,800 + 4,100 LOC respectively.
- **Issues:** Catalog reload on every tick; full DAG rebuild under
  exclusive lock; cluster-wide worker budget shared without
  fairness.
- **Recommendations:** Shmem catalog cache; copy-on-write DAG
  rebuilds; round-robin fair-share dispatch.

### Component: `src/api/`

- **Current state:** Cleanly split into `mod.rs`, `helpers.rs`,
  `diagnostics.rs`, `publication.rs`, `self_monitoring.rs`. The v0.21
  TEST-1/2/3 sweep added 75+ unit tests.
- **Issues:** `publication.rs` and `diagnostics.rs` are the
  current under-tested modules; predictive cost model has no
  robustness guards; bare `pgrx::error!` calls in 7 sites.
- **Recommendations:** TEST-6/7/8 unit-test campaign; add typed
  errors for diagnostics; statistical guards for prediction.

### Component: `src/metrics_server.rs`

- **Current state:** New OpenMetrics endpoint (130 LOC). Zero
  unit tests.
- **Issues:** No tests for port conflict, timeout, malformed HTTP;
  no validation that output conforms to OpenMetrics; per-DB metrics
  endpoint is single-DB only (not cluster-wide).
- **Recommendations:** Unit tests; cluster-wide aggregation view;
  OpenMetrics conformance test.

### Component: `src/template_cache.rs`

- **Current state:** Two-level (L1 thread-local + L2 catalog) cache.
- **Issues:** L1 unbounded; no L0 shmem cache; first-touch latency
  dominant in pooler workloads.
- **Recommendations:** L0 dshash cache; L1 LRU.

### Component: `src/shmem.rs`

- **Current state:** Single `PgLwLock` over the entire shared
  state.
- **Issues:** All accesses go through one exclusive lock; no
  share() reads; lock held during DAG rebuild.
- **Recommendations:** Split into per-concern locks; share() for
  read-mostly fields; copy-on-write for DAG.

### Component: `src/error.rs`

- **Current state:** ~15 typed variants; `raise_error_with_context`
  attaches DETAIL/HINT for the most important variants.
- **Issues:** Diagnostic and publication paths still use bare
  `pgrx::error!` strings.
- **Recommendations:** Add `DiagnosticError` and `PublicationError`
  variants; CI-gate that `ereport!`/`error!` outside `error.rs`
  carries a HINT (per the prior §5.5 recommendation; still not
  implemented).

### Component: `tests/`

- **Current state:** 134 integration files; ~1,300 in-crate tests;
  property tests, fuzz target, soak/multi-DB stability workflows.
- **Issues:** New v0.21–v0.23 modules under-tested; concurrency
  matrix incomplete (ALTER+REFRESH, DROP+REFRESH, parallel-worker
  duplicate-pick); no crash-recovery test for publication;
  predictive-model accuracy unverified.
- **Recommendations:** See §6.

---

## 10. Risk Assessment

Highest data-loss / silent-corruption risks, ranked:

1. **EC-01 join phantom rows** (Critical) — affects any LEFT/RIGHT/
   FULL JOIN under mixed UPDATE/DELETE. Real-world deployments may
   already be silently wrong.
2. **Frontier / buffer non-atomicity + UNLOGGED buffers** (High) —
   crash window can lose committed source rows or duplicate them.
3. **Partitioned-source publication mismatch** (High) — silent ST
   freeze with no error.
4. **TOAST-only updates not detected** (Medium-High) — silent drift
   on jsonb/text/array columns.
5. **Two `unwrap()` calls in CDC** (High operationally — backend
   crash, no data loss).
6. **Predictive model without robustness guards** (Medium) — wrong
   automated mode switches.
7. **Resource exhaustion via unbounded `pgt_refresh_history`** (Medium)
   — eventual database degradation.
8. **DAG-cache lock contention at >500 STs** (Medium) — operability.
9. **Cluster-wide worker starvation across databases** (Medium —
   multi-tenant only).
10. **Untested concurrency matrix (ALTER/DROP/REFRESH)** (Medium —
    likely-but-unverified bugs).

Lower-risk but worth-tracking: cron parser DoS via pathological
input; metrics endpoint OOM under hostile request; SLA tier
oscillation thrash.

---

## 11. Conclusions & Priorities

The v0.21–v0.23 cycle was a strong correctness, observability, and
ergonomics release: most of the prior "easy wins" are gone. The
remaining issues fall into three categories:

1. **A small number of high-impact correctness bugs** (EC-01,
   frontier non-atomicity, partitioned-source freeze, TOAST hashing,
   the two CDC unwraps). All five are scoped, well-understood, and
   can be addressed in v0.24.0. None blocks the v0.24.0 transactional
   outbox/inbox theme — they are complementary.
2. **Scalability ceilings that show up above 200 STs** (catalog reload
   per tick, single PGS_STATE lock, missing indexes, per-backend
   template cache). The fixes are mechanically simple; together they
   would push the comfortable operating point from "hundreds" to
   "thousands" of stream tables on commodity hardware.
3. **Test coverage that has not kept pace with the v0.21–v0.23 surface**
   (predictive cost model, publication, metrics endpoint,
   concurrency matrix). The risk here is reputational — a regression
   in one of the new automation paths is harder to bisect than the
   well-tested core.

**Top five action items** for the maintainers in priority order:

1. **EC01-1..4** — fix the join phantom-row bug; remove the Q15
   skip; add proptest. (Critical correctness.)
2. **Two-phase frontier commit + opt-in logged buffers.** (Closes
   the data-loss class.)
3. **Catalog snapshot in shmem + missing internal indexes + history
   retention.** (Unblocks 1k-ST deployments.)
4. **TEST-6/7/8** — 50+ unit tests covering the v0.21–v0.23 surface.
5. **EC-01 unwrap removal in cdc.rs + ARCH-1B sub-module migration.**
   (Code-quality hygiene; closes the prior assessment's open items.)

These are all sized to fit a single v0.24.0 release alongside the
already-planned transactional inbox/outbox work, and would leave
v0.25.0 and beyond free to focus on the relay CLI, the ecosystem
extensions, and the PGlite path to v1.0.

---

## Appendix — Cross-Reference to Prior Assessment

For each finding from the v0.20.0 baseline
([PLAN_OVERALL_ASSESSMENT.md](PLAN_OVERALL_ASSESSMENT.md)), this
report's status:

| Prior § | Title | v0.23.0 status | This report |
|---|---|---|---|
| 2.1 | EC-01 join phantom rows | **Still unresolved** | §3 Critical #1 |
| 2.2 | 28 parser unwraps | Fixed (SAF-1) | §2 |
| 2.3 | 547 unsafe blocks | Reduced 42% | §2 |
| 2.4 | Monolithic refresh.rs | Submodules created, migration incomplete | §4 ARCH-1B |
| 2.5 | Recursive CTE recomputation | Fully implemented | §2 |
| 2.6 | Non-determinism unstarted | Done (OP-6/ND1–4) | §2 |
| 3.1 | No parallel refresh | Shipped (PAR-1..5) | §2 |
| 3.2 | No downstream CDC | Shipped (CDC-PUB-*) | §2 + §4 (durability gap) |
| 3.3 | Multi-DB isolation | Per-DB isolated; cluster-wide budget shared | §4 |
| 3.6 | Reactive-only self-monitoring | Predictive shipped | §2 + §4 (no guards) |
| 4.3 | `diff_project` allocations | Unchanged | §5 |
| 4.4 | Per-backend template cache | Unchanged | §4 + §5 + §7 |
| 4.5 | Change-buffer full-scan | Unchanged | §5 |
| 5.2-5.4 | Missing API helpers | Shipped | §2 |
| 6.1 | Three big files no tests | TEST-1/2/3 added; new gaps appeared | §6 |
| 6.2 | No fuzz | Parser fuzz added | §2 + §6 (more wanted) |
| 6.3 | No bgworker crash test | Shipped | §2 |
| 6.4 | TPC-H IMMEDIATE allowlist | Q15 still allowlisted | §3 #1 |
| 8.3 | No Prometheus exporter | Shipped (OP-2) | §2 |
| 8.4 | No canary | Shipped | §2 |

Net: the prior report's "P0/P1" items have largely shipped. The
residual P0 (EC-01) and three new high-severity issues identified
here form the natural v0.24.0 critical path.
