# pg_trickle - Overall Project Assessment v8

> **Status:** Assessment report
> **Type:** Repository-wide audit / gap analysis
> **Date:** 2026-04-28
> **Scope:** Current `main` branch at `0.37.0` after the v0.35-v0.37 hardening,
> scheduler split, merge split, pgvector aggregate, trace-propagation, and
> temporal/columnar feature work.
> **Requested output path:** `plans/PLAN_OVERALL_ASSESSMENT_8.md`
> **Method:** Static repository review, prior-assessment comparison, source/test/doc/CI
> verification, and repo-memory review. No product code changes were made.

---

## 1. Executive Summary

pg_trickle has continued to mature quickly since
[PLAN_OVERALL_ASSESSMENT_7.md](PLAN_OVERALL_ASSESSMENT_7.md). The current
tree is now `0.37.0` ([Cargo.toml](../Cargo.toml#L1-L4),
[META.json](../META.json#L1-L5)), with 78 Rust source files, 118,860 lines of
extension code, 142 Rust test files, 90,841 lines of tests, 40 non-archive SQL
install/upgrade scripts, 19 GitHub workflows, 126 GUC definitions, and 132
`#[pg_extern]` SQL-facing definitions. The project is no longer a small
extension with a few risky hot files; it is now a full in-database streaming
platform with CDC, differential view maintenance, a scheduler, distributed
Citus coordination, relay/TUI/dbt integrations, and substantial operational
surface area.

The strongest positive movement since v7 is real:

- [src/scheduler](../src/scheduler/) now has focused modules for Citus, cost,
  worker pool, and tier scheduling instead of a single scheduler file doing
  everything.
- [src/refresh/merge](../src/refresh/merge/) now separates column, conflict,
  delete, insert, and update helpers from the merge entry point.
- Snapshot/restore atomicity, which older assessments treated as critical, is
  no longer open: [src/api/snapshot.rs](../src/api/snapshot.rs#L25-L87)
  contains a `SnapSubTransaction` RAII helper that wraps snapshot and restore
  work in an internal subtransaction, and restore no longer relies on
  `SELECT * EXCEPT` ([src/api/snapshot.rs](../src/api/snapshot.rs#L141-L180)).
- The old inbox/outbox unit-test gap is partially closed: both
  [src/api/inbox.rs](../src/api/inbox.rs#L1038-L1098) and
  [src/api/outbox.rs](../src/api/outbox.rs#L1007-L1062) now have unit tests,
  although the tests cover only helper behavior, not the reliability contract.
- Q15 is now explicitly in the TPC-H IMMEDIATE skip allowlist
  ([tests/e2e_tpch_tests.rs](../tests/e2e_tpch_tests.rs#L122-L142)), which
  turns a previously hidden skip into an acknowledged limitation.
- Upgrade completeness is in the PR gate
  ([.github/workflows/ci.yml](../.github/workflows/ci.yml#L275-L313)); full
  upgrade E2E remains daily/manual because it builds upgrade Docker images
  ([.github/workflows/ci.yml](../.github/workflows/ci.yml#L314-L377)).

The top risks, however, are still serious and in a few cases sharper than v7:

1. **EC-01 join phantom rows remain the highest correctness risk.** Join deltas
   still return `is_deduplicated: false` ([src/dvm/operators/join.rs](../src/dvm/operators/join.rs#L650-L668)),
   PH-D1 cleanup is still documented as conditional
   ([src/refresh/phd1.rs](../src/refresh/phd1.rs#L18-L33)), and repo memory
   still records Q07/Q15 phantom-row behavior. This blocks a credible v1.0
   correctness story for non-trivial joins.

2. **Several hardening features are present but not actually wired.** The
   SQLSTATE classifier exists, but `PgTrickleError::SpiErrorCode` is not
   constructed outside display/error-boundary code ([src/error.rs](../src/error.rs#L89-L96),
   [src/error.rs](../src/error.rs#L227-L245)); SPI errors still usually flow
   through English message matching ([src/error.rs](../src/error.rs#L276-L346)).
   WAL backpressure has an implementation function
   ([src/wal_decoder.rs](../src/wal_decoder.rs#L456-L493)), but no call site
   in `src/` invokes it. `pg_trickle.history_prune_interval_seconds` is
   defined, but the scheduler uses a hard-coded daily cleanup interval
   ([src/scheduler/mod.rs](../src/scheduler/mod.rs#L2394-L2401),
   [src/scheduler/mod.rs](../src/scheduler/mod.rs#L2768-L2806)).

3. **Event-driven wake is a false-confidence area.** Current code deliberately
   disables it in the background worker because PostgreSQL does not allow
   `LISTEN` there ([src/scheduler/mod.rs](../src/scheduler/mod.rs#L2434-L2455)),
   and the GUC default in code is `false`
   ([src/config.rs](../src/config.rs#L359-L374)). Documentation still says the
   default is `true` and promises approximately 15 ms latency
   ([docs/CONFIGURATION.md](../docs/CONFIGURATION.md#L260-L279),
   [docs/SCALING.md](../docs/SCALING.md#L160-L176)). The E2E latency test uses
   a 10-second threshold despite a 5-second poll interval, so poll-only mode can
   still pass ([tests/e2e_wake_tests.rs](../tests/e2e_wake_tests.rs#L159-L213)).

4. **Citus support is broad but not chaos-tested.** No `tests/**/*citus*.rs`
   file exists. The codebase has Citus modules and workflows, but no automated
   multi-node failure rig for worker death, coordinator restart, lease expiry,
   rebalance churn, or split-brain. This remains the largest distributed
   correctness gap.

5. **v0.37 documentation has drift.** [docs/CONFIGURATION.md](../docs/CONFIGURATION.md)
   does not document the v0.37 trace/vector GUCs found in
   [src/config.rs](../src/config.rs#L1390-L1404) and
   [CHANGELOG.md](../CHANGELOG.md#L74-L142). [docs/UPGRADING.md](../docs/UPGRADING.md#L760-L846)
   stops detailed upgrade guidance and its supported path table at `0.34.0`,
   despite the codebase being `0.37.0`. There is a pgvector tutorial
   ([docs/tutorials/PGVECTOR_EMBEDDING_PIPELINES.md](../docs/tutorials/PGVECTOR_EMBEDDING_PIPELINES.md)),
   but no comparable OpenTelemetry/Jaeger/Tempo guide was found.

The project is impressive and unusually well tested for a PostgreSQL
extension, but it is not yet at the stated "world-class correctness and
durability" bar. The next hardening wave should focus less on adding surface
area and more on closing wiring gaps, proving DVM convergence, making
operational docs truthful, and adding failure-injection coverage for the
distributed and durability paths.

---

## 2. Method and Scope

This assessment began with the historical baselines requested in the prompt:

- [PLAN_OVERALL_ASSESSMENT.md](PLAN_OVERALL_ASSESSMENT.md) - v0.20.0 baseline.
- [PLAN_OVERALL_ASSESSMENT_2.md](PLAN_OVERALL_ASSESSMENT_2.md) - v0.23.0 follow-up.
- [PLAN_OVERALL_ASSESSMENT_3.md](PLAN_OVERALL_ASSESSMENT_3.md) - v0.27.0 follow-up.
- [PLAN_OVERALL_ASSESSMENT_7.md](PLAN_OVERALL_ASSESSMENT_7.md) - v0.34.0 follow-up.
- Repo memory:
  `/memories/repo/df-cdc-buffer-trends-fix.md`,
  `/memories/repo/join-delta-phantom-rows.md`, and
  `/memories/repo/tpch-dvm-scaling-analysis.md`.

I then verified current state against primary sources:

- Core Rust under [src](../src/), especially DVM, refresh, CDC/WAL, scheduler,
  Citus, configuration, error handling, monitor/metrics, snapshot/restore,
  inbox/outbox, and trace/vector additions.
- SQL install and upgrade scripts under [sql](../sql/).
- Tests under [tests](../tests/), including DVM operator tests, E2E, TPC-H,
  SQLancer, property tests, pgvector, trace, wake, upgrade, and stability files.
- Fuzz targets under [fuzz/fuzz_targets](../fuzz/fuzz_targets/), benchmarks under
  [benches](../benches/), and proptest regressions under
  [proptest-regressions](../proptest-regressions/).
- Docs under [docs](../docs/), plus [README.md](../README.md),
  [INSTALL.md](../INSTALL.md), [CHANGELOG.md](../CHANGELOG.md),
  [ROADMAP.md](../ROADMAP.md), and [ESSENCE.md](../ESSENCE.md).
- Workflows under [.github/workflows](../.github/workflows/).
- Supporting integrations: [pgtrickle-relay](../pgtrickle-relay/),
  [pgtrickle-tui](../pgtrickle-tui/), [dbt-pgtrickle](../dbt-pgtrickle/),
  [monitoring](../monitoring/), [cnpg](../cnpg/), scripts, Dockerfiles, and
  release metadata.

No tests or benchmarks were run as part of this assessment. The findings below
are static-analysis findings, evidence-backed by source and repo structure. If
something is a plausibility risk rather than proven runtime failure, it is
labelled as such.

---

## 3. What Has Improved Since Prior Assessments

### 3.1 Fixed or Substantially Mitigated Since v7

| Prior issue | Current state | Evidence |
|---|---|---|
| Snapshot/restore atomicity | **Fixed.** Internal subtransaction wrapper with panic-safe rollback now brackets snapshot and restore operations. | [src/api/snapshot.rs](../src/api/snapshot.rs#L25-L87), [src/api/snapshot.rs](../src/api/snapshot.rs#L265-L314), [src/api/snapshot.rs](../src/api/snapshot.rs#L383-L412) |
| `SELECT * EXCEPT` PG-minor sensitivity in restore | **Fixed.** Restore now builds explicit storage/snapshot column lists from `pg_attribute`. | [src/api/snapshot.rs](../src/api/snapshot.rs#L141-L180), [src/api/snapshot.rs](../src/api/snapshot.rs#L408-L412) |
| Inbox/outbox no unit tests | **Partially fixed.** Unit-test blocks exist, but only cover partition/name helpers. | [src/api/inbox.rs](../src/api/inbox.rs#L1038-L1098), [src/api/outbox.rs](../src/api/outbox.rs#L1007-L1062) |
| Q15 IMMEDIATE hidden failure | **Mitigated.** Q15 is now explicitly allowlisted with a comment explaining stale-row behavior. | [tests/e2e_tpch_tests.rs](../tests/e2e_tpch_tests.rs#L122-L142) |
| `force_full_refresh` missing | **Implemented.** The GUC exists and API mode reporting recognizes it. | [src/config.rs](../src/config.rs#L1240-L1248), [src/api/mod.rs](../src/api/mod.rs#L5265-L5266) |
| History retention not batched | **Partially fixed.** Daily cleanup deletes 10,000-row batches. | [src/scheduler/mod.rs](../src/scheduler/mod.rs#L2768-L2806) |
| Scheduler and merge monoliths | **Improved.** Scheduler and merge have submodules, though large files remain. | [src/scheduler](../src/scheduler/), [src/refresh/merge](../src/refresh/merge/) |
| v0.37 pgvector aggregate support | **Added and tested.** E2E covers insert/update/delete/sum/HNSW cases. | [tests/e2e_pgvector_tests.rs](../tests/e2e_pgvector_tests.rs#L1-L260) |
| Trace context capture | **Added with integration tests.** Change-buffer column, GUC capture, and refresh success are tested. | [tests/integration_trace_tests.rs](../tests/integration_trace_tests.rs#L1-L190) |
| Upgrade completeness PR gate | **Present.** PRs run a lightweight version/script completeness check. | [.github/workflows/ci.yml](../.github/workflows/ci.yml#L275-L313) |

### 3.2 Improvements That Are Real but Need Qualification

- **L0 cache:** v0.36 added a process-local L0 map
  ([src/shmem.rs](../src/shmem.rs#L841-L890)). This is better than an empty
  signal, but it is not a shared dshash cache; cross-backend sharing still
  depends on the L2 catalog table ([src/template_cache.rs](../src/template_cache.rs#L4-L16)).
- **SQLSTATE classifier:** the classifier itself is implemented
  ([src/error.rs](../src/error.rs#L348-L420)), but no source path constructs
  `PgTrickleError::SpiErrorCode` for actual SPI failures. This is a partial
  implementation, not a completed migration.
- **WAL backpressure:** `is_backpressure_active()` exists
  ([src/wal_decoder.rs](../src/wal_decoder.rs#L456-L493)), but no call site
  invokes it. The GUC and docs therefore overstate current protection.
- **Event-driven wake:** CDC triggers emit `pg_notify('pgtrickle_wake', '')`
  ([src/cdc.rs](../src/cdc.rs#L1755-L1790)), but scheduler LISTEN is disabled
  in the background worker ([src/scheduler/mod.rs](../src/scheduler/mod.rs#L2434-L2455)).
  NOTIFY emission is not the same as event-driven scheduling.

---

## 4. Critical Findings

### Finding C1 - EC-01 join phantom rows remain unresolved

**Severity:** Critical

**Evidence:**

- Join deltas still return `is_deduplicated: false` for all joins
  ([src/dvm/operators/join.rs](../src/dvm/operators/join.rs#L650-L668)).
- PH-D1 cleanup is documented as conditional on join shape, prior residuals,
  and refresh count ([src/refresh/phd1.rs](../src/refresh/phd1.rs#L18-L33)).
- Repo memory `/memories/repo/join-delta-phantom-rows.md` documents active
  Q07 and Q15 phantom-row behavior tied to Part 1a/1b row-id divergence and
  current-delta-only weight aggregation.
- Q15 is in `IMMEDIATE_SKIP_ALLOWLIST` because it still produces an extra row
  in IMMEDIATE mode ([tests/e2e_tpch_tests.rs](../tests/e2e_tpch_tests.rs#L136-L142)).

**Why it matters:** pg_trickle's core product promise is that differential
refresh produces the same result as full refresh. A join-delta phantom is a
silent wrong-result bug in exactly the class of queries users will expect an
IVM engine to handle: multi-table joins feeding aggregate or operational
dashboards.

**User/operator impact:** Incorrect rows can accumulate across cycles, skew
aggregates, trigger downstream duplicate-key behavior, or force operators to
fall back to FULL refresh for important join workloads.

**Recommended fix direction:**

- Make PH-D1 cleanup unconditional for non-deduplicated join outputs while the
  root fix is developed, with a bounded batch size and explicit monitoring.
- Wire the v0.36 `RowIdSchema` declarations into the DVM planning path so row-id
  compatibility is verified before execution.
- Add an EC-01-specific property generator with tens of thousands of random
  insert/update/delete sequences across left/right co-deletion and scalar
  subquery cases.
- Revisit the join row-id formula so Part 1a, Part 1b, and Part 2 converge on
  the same logical identity across pre/post images.

**Priority/window:** P0, before any v1.0 GA claim. This should be the lead item
for v0.38.0 or a dedicated correctness sprint.

### Finding C2 - WAL backpressure is advertised but not enforced

**Severity:** High

**Evidence:**

- `pg_trickle.enforce_backpressure` is defined in
  [src/config.rs](../src/config.rs#L1323-L1331) and documented as suppressing
  writes when slot lag crosses the critical threshold
  ([src/config.rs](../src/config.rs#L2915-L2923)).
- `wal_decoder::is_backpressure_active()` implements a check
  ([src/wal_decoder.rs](../src/wal_decoder.rs#L456-L493)).
- A source-wide search finds no call site for `is_backpressure_active()` outside
  its own definition. CDC trigger generation checks only `pg_trickle.cdc_paused`
  ([src/cdc.rs](../src/cdc.rs#L1755-L1790)).
- The function comment says backpressure releases below 50 percent, but the
  function is stateless and returns `false` for any lag below the critical
  threshold ([src/wal_decoder.rs](../src/wal_decoder.rs#L475-L493)).

**Why it matters:** WAL slot lag can fill disks. A GUC that appears to protect
operators from slot-lag explosions but is not wired creates a dangerous false
sense of safety.

**User/operator impact:** Operators may turn on `pg_trickle.enforce_backpressure`
under incident pressure and believe source DML is being suppressed, while CDC
triggers keep writing and WAL retention keeps growing.

**Recommended fix direction:**

- Decide whether suppression belongs in trigger-generated PL/pgSQL, scheduler
  gating, or a durable pause state. If in triggers, the trigger needs an
  inexpensive way to know per-source slot lag without running expensive slot
  queries per row.
- Add stateful hysteresis, not a one-shot threshold check.
- Add E2E tests that configure a low `slot_lag_critical_threshold_mb`, simulate
  lag, and assert writes are paused or explicitly not paused depending on mode.
- Document the data-loss implications clearly: suppressing CDC writes is not a
  neutral throttle unless the system also marks the stream table for full
  reinitialization or holds source writes outside pg_trickle.

**Priority/window:** P0/P1. If the feature cannot be made safe quickly, mark the
GUC experimental and update docs immediately.

### Finding C3 - Event-driven wake is disabled but documented and tested as if active

**Severity:** High

**Evidence:**

- The scheduler forces `let event_driven = false` even when the GUC is enabled,
  with a warning that background workers cannot `LISTEN`
  ([src/scheduler/mod.rs](../src/scheduler/mod.rs#L2434-L2455)).
- The code default for `PGS_EVENT_DRIVEN_WAKE` is `false`
  ([src/config.rs](../src/config.rs#L359-L374)).
- [docs/CONFIGURATION.md](../docs/CONFIGURATION.md#L260-L279),
  [docs/SCALING.md](../docs/SCALING.md#L160-L176), and
  [docs/TROUBLESHOOTING.md](../docs/TROUBLESHOOTING.md#L748-L762) describe it
  as enabled/default or as a low-latency profile knob.
- The latency test in [tests/e2e_wake_tests.rs](../tests/e2e_wake_tests.rs#L159-L213)
  configures a 5-second poll interval but accepts completion within 10 seconds,
  so a poll-only scheduler can pass.

**Why it matters:** Latency is a major product claim. Documentation and tests
that imply sub-50 ms wake while code is poll-only mislead maintainers and users.

**User/operator impact:** Users tuning for dashboards or operational monitoring
will set `event_driven_wake = on` and still see poll-bound latency. They may
then over-tune `scheduler_interval_ms`, increasing scheduler load.

**Recommended fix direction:**

- Update documentation to say the GUC is reserved and currently non-functional
  in background workers, or implement a background-worker-compatible wake path
  via latch/shared memory rather than LISTEN.
- Change the E2E test to assert refresh latency is strictly below the poll
  interval by a meaningful margin, or skip it while the feature is reserved.
- Add a regression test for the warning path when users enable the GUC.

**Priority/window:** P0 for docs/test truthfulness; P1/P2 for a real latch-based
implementation.

### Finding C4 - SQLSTATE classification remains mostly unwired

**Severity:** High

**Evidence:**

- `PgTrickleError::SpiErrorCode(u32, String)` exists
  ([src/error.rs](../src/error.rs#L89-L96)).
- `is_retryable()` uses SQLSTATE classification only for that variant
  ([src/error.rs](../src/error.rs#L227-L245)).
- The legacy `SpiError(String)` path still matches English message text
  ([src/error.rs](../src/error.rs#L276-L346)).
- A source search finds `SpiErrorCode(` only in display/error definitions and
  boundary formatting, not in actual SPI error construction.

**Why it matters:** Locale-dependent retry classification is fragile. Permanent
errors can be retried until suspension, and transient errors can be mishandled
when PostgreSQL message text differs by locale or version.

**User/operator impact:** On non-English PostgreSQL builds or changed message
wording, scheduler retry decisions become noisy and misleading. Operators see
retries and fuses rather than direct "permission denied" or "undefined table"
classification.

**Recommended fix direction:**

- Introduce one conversion helper for pgrx SPI errors that extracts
  `ErrorData.sqlerrcode` where available and constructs `SpiErrorCode`.
- Replace ad hoc `PgTrickleError::SpiError(e.to_string())` call sites on
  scheduler/refresh/catalog hot paths.
- Add tests that simulate SQLSTATE classes and, if possible, an integration
  test with localized `lc_messages`.

**Priority/window:** P1, before v1.0.

### Finding C5 - Citus distributed coordination lacks hardening coverage

**Severity:** High

**Evidence:**

- No `tests/**/*citus*.rs` file exists.
- Citus code is significant: [src/citus.rs](../src/citus.rs) plus
  [src/scheduler/citus.rs](../src/scheduler/citus.rs).
- CI has CNPG, dbt, upgrade, TPC-H, SQLancer, and stability jobs, but no Citus
  topology/chaos workflow ([.github/workflows](../.github/workflows/)).

**Why it matters:** Single-node correctness tests cannot prove distributed CDC
coordination. Citus failure modes are about leases, workers, topology changes,
logical slots, timing, and split-brain, not ordinary SQL equivalence.

**User/operator impact:** A production Citus deployment can hit worker death,
coordinator restart, shard rebalance, or lease expiry paths that have never
been exercised by CI. The risk is either silent staleness or duplicate/missed
change processing.

**Recommended fix direction:**

- Add a Citus test rig with coordinator + at least 2 workers.
- Test worker kill/restart during polling, coordinator restart mid-lease,
  rebalance churn, stale `pgt_worker_slots` cleanup, and concurrent scheduler
  lease contention.
- Expose retry/skipped-worker counters in metrics and assert them in tests.

**Priority/window:** P1 for any serious distributed support claim.

---

## 5. Correctness and Reliability Gaps

### Finding R1 - `RowIdSchema` is documentation unless enforced

**Severity:** High

**Evidence:** [src/dvm/row_id.rs](../src/dvm/row_id.rs#L23-L119) defines
`RowIdSchema` and `verify_pipeline()`, but source search only finds the verifier
in its own tests ([src/dvm/row_id.rs](../src/dvm/row_id.rs#L286-L312)).

**Why it matters:** v0.36 correctly names the structural problem behind EC-01,
but the type does not yet prevent an incompatible operator pipeline from being
planned. A safety type that is not wired into the planner is a design note, not
a guardrail.

**Impact:** Future DVM operators can repeat row-id compatibility mistakes
without a plan-time failure.

**Fix direction:** Require every `DiffResult` or operator node to carry a
`RowIdSchema`, collect schemas during `diff_node()` traversal, and fail creation
or force FULL when incompatible schemas are found.

**Priority/window:** P1, in the same sprint as EC-01.

### Finding R2 - CDC pause and backpressure suppression semantics risk data loss

**Severity:** High

**Evidence:** CDC triggers return without recording a change when
`pg_trickle.cdc_paused = on` ([src/cdc.rs](../src/cdc.rs#L272-L275),
[src/cdc.rs](../src/cdc.rs#L1763-L1790)). WAL backpressure, if wired the same
way, would also suppress change capture.

**Why it matters:** A pause knob can mean either "stop refresh but keep durable
change capture" or "drop changes and require reinitialization". Current trigger
behavior is the latter unless every affected stream table is later full
refreshed.

**Impact:** During an incident, operators may pause CDC to reduce pressure and
later resume expecting no data loss. In reality, source DML that occurred while
paused was not captured.

**Fix direction:** Split the semantics into two explicit modes: durable hold
(capture changes but do not refresh) and discarding pause (suppress capture and
mark affected stream tables for reinit). Document both and add tests.

**Priority/window:** P1 for operational safety.

### Finding R3 - Trace propagation captures context but lacks live export proof

**Severity:** Medium

**Evidence:** Integration tests verify `__pgt_trace_context` capture and refresh
success ([tests/integration_trace_tests.rs](../tests/integration_trace_tests.rs#L1-L190)),
and unit-style checks validate payload shape. The file explicitly notes live
OTLP/Jaeger export needs an ignored/local setup
([tests/integration_trace_tests.rs](../tests/integration_trace_tests.rs#L8-L13)).

**Why it matters:** The feature claim is distributed trace propagation through
CDC -> DVM -> MERGE. Capturing the traceparent in a buffer is necessary but not
the same as verifying export to an OTLP collector, retry behavior, timeout
behavior, or endpoint failures.

**Impact:** Operators may enable trace propagation and discover only in
production that collector outages block, spam logs, or silently drop spans.

**Fix direction:** Add a dockerized OTEL Collector/Jaeger E2E test, endpoint
failure tests, timeout tests, and docs for collector configuration.

**Priority/window:** P2, before advertising tracing as production-grade.

### Finding R4 - TRUNCATE behavior remains a user-facing semantic edge

**Severity:** Medium

**Evidence:** TRUNCATE trigger functions exist and write an action-only record
([src/cdc.rs](../src/cdc.rs#L250-L285)), but user docs still warn that row-level
triggers do not fire on TRUNCATE and recommend manual/full refresh in some
places ([docs/GETTING_STARTED.md](../docs/GETTING_STARTED.md#L339-L339)).

**Why it matters:** TRUNCATE is a common operational action during reloads.
Conflicting docs and implementation details make it unclear whether all
refresh modes handle it incrementally, as a full invalidation, or via manual
operator intervention.

**Impact:** Users may over-trust or under-use TRUNCATE support depending on
which doc they read.

**Fix direction:** Consolidate TRUNCATE semantics in SQL_REFERENCE and tutorials:
what is captured, which refresh modes are valid, whether FULL is forced, and
what happens to downstream DAG nodes.

**Priority/window:** P2.

---

## 6. Performance and Scalability Gaps

### Finding P1 - TPC-H DVM scaling is not yet world-class

**Severity:** High

**Evidence:** Repo memory `/memories/repo/tpch-dvm-scaling-analysis.md` records
SF=0.01, SF=0.1, and SF=1.0 runs with threshold-collapse patterns for Q05,
Q07, Q08, Q09, Q22, early collapse for Q04, and structural slowness for Q20.
The current join operator still contains a deep-join L0 threshold
([src/dvm/operators/join.rs](../src/dvm/operators/join.rs#L104-L104)), which is
itself evidence that deep join SQL generation remains a spill-risk area.

**Why it matters:** A best-in-class DVM engine should make common analytic
joins cheaper than full refresh, not fall into multi-minute or temp-file-limit
paths at moderate scale.

**Impact:** Users with TPC-H-like schemas may find DIFF mode slower or less
reliable than FULL for the very queries that motivate pg_trickle adoption.

**Fix direction:** Capture generated delta SQL and `EXPLAIN (ANALYZE, BUFFERS)`
for Q04/Q05/Q07/Q08/Q09/Q20/Q22 at SF=0.1 and SF=1.0. Measure with larger
`work_mem` to distinguish PostgreSQL spill from DVM SQL shape. Add per-query
benchmark artifacts to CI for trend analysis.

**Priority/window:** P1/P2, depending on v1.0 performance claims.

### Finding P2 - Process-local L0 cache is useful but not the shared cache the name implies

**Severity:** Medium

**Evidence:** [src/shmem.rs](../src/shmem.rs#L820-L832) explains that L0 is a
process-local `RwLock<HashMap>` and that L2 catalog entries provide
cross-backend sharing. The actual map is a `OnceLock<RwLock<HashMap<...>>>`
([src/shmem.rs](../src/shmem.rs#L841-L890)).

**Why it matters:** Connection-pooler and short-lived backend workloads still
pay per-backend warmup. The design is better than v7's empty signal, but it is
not a shared-memory template cache.

**Impact:** Operators may size latency expectations based on a fully shared L0
cache and still see first-touch catalog lookup overhead per backend.

**Fix direction:** Either rename/document the cache as process-local, or build
the originally described shared dshash/dynamic shared-memory cache with bounded
memory and invalidation semantics.

**Priority/window:** P2.

### Finding P3 - History pruning interval GUC is unused

**Severity:** Medium

**Evidence:** `pg_trickle.history_prune_interval_seconds` is defined and exposed
([src/config.rs](../src/config.rs#L1271-L1277),
[src/config.rs](../src/config.rs#L2902-L2906)), but the scheduler uses
`const HISTORY_CLEANUP_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000`
([src/scheduler/mod.rs](../src/scheduler/mod.rs#L2394-L2401)). A source search
finds `pg_trickle_history_prune_interval_seconds()` only in
[src/config.rs](../src/config.rs#L3090-L3091).

**Why it matters:** Operators tuning history table growth will reasonably
expect the interval GUC to control cleanup cadence.

**Impact:** Large deployments can accumulate far more history than expected
between daily runs even if they set the interval to 60 seconds.

**Fix direction:** Use the GUC in scheduler cleanup cadence, or remove it and
document daily cleanup. Prefer a dedicated pruner or a bounded per-tick budget.

**Priority/window:** P2.

### Finding P4 - Benchmark gates are broad but still miss per-query p99 and generated-SQL diagnosis

**Severity:** Medium

**Evidence:** The repo has Criterion benches ([benches](../benches/)), E2E
benchmarks ([.github/workflows/e2e-benchmarks.yml](../.github/workflows/e2e-benchmarks.yml)),
and TPC-H nightly ([.github/workflows/tpch-nightly.yml](../.github/workflows/tpch-nightly.yml#L42-L84)).
The benchmark workflow extracts TPC-H benchmark text artifacts
([.github/workflows/e2e-benchmarks.yml](../.github/workflows/e2e-benchmarks.yml#L319-L350)),
but there is no first-class artifact containing generated delta SQL plus
`EXPLAIN` plans for regressions.

**Why it matters:** Pass/fail correctness and coarse timing do not explain why
a differential query crosses a spill threshold.

**Impact:** Performance regressions can be found late and require manual
reproduction rather than being diagnosable from CI artifacts.

**Fix direction:** Emit structured JSON/CSV per query, refresh mode, phase,
delta row count, temp blocks, work_mem, and generated SQL hash. Archive EXPLAIN
for query families known to collapse.

**Priority/window:** P2.

---

## 7. Code Quality and Maintainability Gaps

### Finding M1 - Large-file risk remains despite improved modularity

**Severity:** Medium

**Evidence:** Largest source files as of this audit:

| File | Lines |
|---|---:|
| [src/scheduler/mod.rs](../src/scheduler/mod.rs) | 7,428 |
| [src/api/mod.rs](../src/api/mod.rs) | 7,026 |
| [src/dvm/parser/sublinks.rs](../src/dvm/parser/sublinks.rs) | 6,559 |
| [src/dvm/parser/rewrites.rs](../src/dvm/parser/rewrites.rs) | 6,112 |
| [src/dvm/parser/mod.rs](../src/dvm/parser/mod.rs) | 5,526 |
| [src/dvm/operators/aggregate.rs](../src/dvm/operators/aggregate.rs) | 5,400 |
| [src/config.rs](../src/config.rs) | 4,405 |
| [src/dag.rs](../src/dag.rs) | 4,295 |
| [src/cdc.rs](../src/cdc.rs) | 4,107 |

**Why it matters:** The scheduler split and merge split reduced some change
risk, but critical paths still concentrate large amounts of logic in files
that are hard to review completely.

**Impact:** Future correctness fixes, especially around scheduler/Citus/DVM,
will be slower and riskier than necessary.

**Fix direction:** Continue extracting `api/mod.rs` into lifecycle, refresh,
status, fuse, bulk, and temporal/vector APIs. Split parser rewrites by rewrite
family. Move scheduler loop state machines into smaller testable units.

**Priority/window:** P2.

### Finding M2 - Unsafe surface remains large and hard to audit mechanically

**Severity:** Medium

**Evidence:** Approximate static count found 364 non-comment `unsafe` keyword
lines in [src](../src/). Parser files dominate the unsafe surface. The project
does have safety comments and an unsafe-inventory workflow
([.github/workflows/unsafe-inventory.yml](../.github/workflows/unsafe-inventory.yml)),
but that workflow is manual-only.

**Why it matters:** pgrx parser FFI needs unsafe code, but unsafe drift should
be visible on every PR that touches parser/FFI code.

**Impact:** New unsafe blocks can be introduced without a mandatory inventory
diff in the PR gate.

**Fix direction:** Run unsafe inventory on PRs touching `src/dvm/parser/**`,
`src/shmem.rs`, `src/api/helpers.rs`, `src/wal_decoder.rs`, and `src/lib.rs`.
Fail if a new unsafe block lacks a `SAFETY:` comment.

**Priority/window:** P2.

### Finding M3 - Thin helper tests can mask reliability-contract gaps

**Severity:** Medium

**Evidence:** Inbox unit tests cover partition hashing
([src/api/inbox.rs](../src/api/inbox.rs#L1038-L1098)); outbox unit tests cover
table-name generation ([src/api/outbox.rs](../src/api/outbox.rs#L1007-L1062)).
The actual inbox/outbox reliability surface includes dedup keys, envelope
versions, consumer offsets, leases, replay behavior, NULL payloads, and
concurrency.

**Why it matters:** v7's "no unit tests" item is technically fixed, but the
risk that motivated it is not fully addressed.

**Impact:** At-least-once/dedup promises rely on slow E2E coverage and manual
reasoning rather than fast deterministic unit/property tests.

**Fix direction:** Add pure helper functions around envelope encoding/decoding,
dedup-key validation, lease arithmetic, and offset transitions, then test them
without PostgreSQL. Keep E2E for transactional integration.

**Priority/window:** P2.

---

## 8. Testing and CI Coverage Gaps

### Finding T1 - Event-driven wake test does not prove event-driven wake

**Severity:** High

**Evidence:** [tests/e2e_wake_tests.rs](../tests/e2e_wake_tests.rs#L159-L213)
claims a scheduler with `scheduler_interval_ms = 5000` should complete via
NOTIFY much faster than the poll interval, but it waits up to 10 seconds and
asserts only `elapsed < 10s`.

**Why it matters:** This is a classic false-confidence test. It can pass while
the code path under test is disabled.

**Impact:** Maintainers may believe event-driven latency is protected by CI.

**Fix direction:** Until event-driven wake works, rewrite the test to assert the
warning/poll-only behavior. Once implemented, assert a bound below the poll
interval, for example `< 2s` with a 5s interval and enough margin for CI.

**Priority/window:** P0 for test truthfulness.

### Finding T2 - No Citus E2E/chaos test tier

**Severity:** High

**Evidence:** No `tests/**/*citus*.rs` file exists. Citus behavior is too
specific to be covered by ordinary PostgreSQL/Testcontainers tests.

**Why it matters:** Distributed source CDC is among the highest-risk areas and
cannot be validated by static checks or single-node tests.

**Impact:** Release confidence for v0.32-v0.34 Citus claims is lower than the
roadmap suggests.

**Fix direction:** Create `tests/e2e_citus_tests.rs` plus a workflow that can
run daily/manual. Start with topology setup and lease acquisition; then add
chaos scenarios.

**Priority/window:** P1.

### Finding T3 - Fuzzing is still concentrated in pure parsing/config helpers

**Severity:** Medium

**Evidence:** Current fuzz targets are
[cdc_fuzz.rs](../fuzz/fuzz_targets/cdc_fuzz.rs),
[cron_fuzz.rs](../fuzz/fuzz_targets/cron_fuzz.rs),
[guc_fuzz.rs](../fuzz/fuzz_targets/guc_fuzz.rs), and
[parser_fuzz.rs](../fuzz/fuzz_targets/parser_fuzz.rs). There is no fuzz target
for WAL decoder parsing, merge SQL template generation, DAG SCC/topology, or
snapshot column-list generation.

**Why it matters:** The highest-impact correctness failures are often in state
machines and code generation, not only user input parsers.

**Impact:** Core paths with unsafe/parser/SQL-generation complexity rely on
unit/E2E tests but not randomized adversarial coverage.

**Fix direction:** Add fuzz targets for WAL change payloads, merge template
inputs, DAG graph mutations, and snapshot/restore identifier/column shapes.

**Priority/window:** P2.

### Finding T4 - Full upgrade E2E is daily/manual only

**Severity:** Medium

**Evidence:** Upgrade completeness runs on PRs
([.github/workflows/ci.yml](../.github/workflows/ci.yml#L275-L313)), but true
upgrade E2E jobs run only on schedule/workflow dispatch
([.github/workflows/ci.yml](../.github/workflows/ci.yml#L314-L377)).

**Why it matters:** This is a reasonable cost trade-off, but SQL-facing changes
can merge before full upgrade behavior is tested.

**Impact:** Upgrade regressions may be discovered after merge rather than before.

**Fix direction:** Add a PR path filter: run a small upgrade E2E matrix when
`sql/**`, `src/lib.rs`, `src/config.rs`, `src/cdc.rs`, or SQL-exposed API files
change. Keep full all-pairs matrix daily/manual.

**Priority/window:** P2.

---

## 9. SQL/API/UX and Documentation Gaps

### Finding U1 - CONFIGURATION.md is out of sync with code

**Severity:** High

**Evidence:**

- Code default for `pg_trickle.event_driven_wake` is `false`
  ([src/config.rs](../src/config.rs#L359-L374)), but
  [docs/CONFIGURATION.md](../docs/CONFIGURATION.md#L260-L279) says default
  `true` and describes active LISTEN/NOTIFY wake.
- `pg_trickle.enable_trace_propagation`, `pg_trickle.otel_endpoint`,
  `pg_trickle.trace_id`, and `pg_trickle.enable_vector_agg` are in
  [CHANGELOG.md](../CHANGELOG.md#L74-L142) and [src/config.rs](../src/config.rs),
  but searches of [docs/CONFIGURATION.md](../docs/CONFIGURATION.md) did not
  find their configuration sections.

**Why it matters:** Configuration docs are operational controls. Stale values
cause wrong tuning and incident-response mistakes.

**Impact:** Operators may enable knobs that do not work, miss new v0.37 knobs,
or apply inappropriate latency profiles.

**Fix direction:** Generate CONFIGURATION.md from `GucRegistry` metadata or add
a CI check comparing `src/config.rs` GUC names against docs.

**Priority/window:** P0/P1 for event-driven/backpressure truth; P2 for full
generation.

### Finding U2 - UPGRADING.md stops at 0.34.0 while current is 0.37.0

**Severity:** Medium

**Evidence:** Detailed guidance ends at `0.33.0 -> 0.34.0`, and the supported
path table ends at `0.33.0 -> 0.34.0`
([docs/UPGRADING.md](../docs/UPGRADING.md#L760-L846)). Current release metadata
is `0.37.0`.

**Why it matters:** Upgrade documentation is part of production readiness,
especially for an extension with 40 non-archive SQL scripts.

**Impact:** Operators upgrading through v0.35-v0.37 lack a single human-readable
summary of schema changes, restart needs, GUC changes, trace-context column
addition, and pgvector enablement.

**Fix direction:** Add v0.34 -> v0.35, v0.35 -> v0.36, and v0.36 -> v0.37
sections and update the supported path table. Add a docs check that latest
upgrade target matches `Cargo.toml`.

**Priority/window:** P1.

### Finding U3 - OpenTelemetry documentation is missing compared with feature surface

**Severity:** Medium

**Evidence:** Trace code and tests exist ([src/otel.rs](../src/otel.rs),
[tests/integration_trace_tests.rs](../tests/integration_trace_tests.rs)), but no
dedicated docs page for OpenTelemetry/OTLP/Jaeger/Tempo setup was found under
[docs](../docs/).

**Why it matters:** Trace propagation is operationally useful only if operators
can wire it into their collector stack safely.

**Impact:** Users must infer endpoint format, timeout behavior, and failure
mode from code.

**Fix direction:** Add `docs/integrations/opentelemetry.md` or similar with
collector config, Jaeger/Tempo examples, GUCs, expected spans, security notes,
and troubleshooting.

**Priority/window:** P2.

### Finding U4 - Need a generated SQL/API surface inventory

**Severity:** Medium

**Evidence:** Current static count found 132 `#[pg_extern]` definitions and 126
GUC definitions. Docs are extensive, but manually maintained references lag new
surface area.

**Why it matters:** With this many SQL functions and GUCs, manual docs will
continue to drift.

**Impact:** Users discover important functions through source or changelog
rather than the reference docs.

**Fix direction:** Generate API and GUC inventories from source metadata and
fail CI when public entries are absent from SQL_REFERENCE/CONFIGURATION.

**Priority/window:** P2.

---

## 10. Security and Safety Findings

### Finding S1 - SECURITY DEFINER search_path discipline looks good, but docs should state the contract

**Severity:** Low/Medium

**Evidence:** CDC trigger functions use `SECURITY DEFINER` with explicit
`SET search_path = pgtrickle_changes, pgtrickle, pg_catalog, pg_temp`
([src/cdc.rs](../src/cdc.rs#L250-L285), [src/cdc.rs](../src/cdc.rs#L1755-L1790)).

**Why it matters:** This is good hardening, but the privilege model should be
obvious to security reviewers: who can create stream tables, who owns source
tables, and what RLS/security-definer behavior is intentional.

**Impact:** Adoption reviews may stall if privilege boundaries require source
reading.

**Fix direction:** Add a dedicated security model section covering invoker vs
definer functions, search_path, CDC buffer schema access, RLS behavior, and
required grants.

**Priority/window:** P2.

### Finding S2 - Supply-chain trust lacks signing/SBOM/provenance

**Severity:** Medium

**Evidence:** No workflow references to `cosign`, `cyclonedx`, `cargo-sbom`,
`slsa`, or provenance attestation were found in [.github/workflows](../.github/workflows/).
Release workflows build PGXN/Docker artifacts, but signing/attestation is not
visible.

**Why it matters:** A PostgreSQL extension loaded into the database process has
a high trust bar. Production operators increasingly require SBOMs and signed
artifacts.

**Impact:** Enterprise or regulated deployments may block adoption even if the
code is technically strong.

**Fix direction:** Publish CycloneDX/SPDX SBOMs, sign Docker images with cosign,
attach SLSA provenance, and document verification.

**Priority/window:** P2 before v1.0.

### Finding S3 - TUI/relay secret handling needs explicit operator guidance

**Severity:** Medium

**Evidence:** Relay has environment-variable interpolation tests and config
support; TUI connection strings can still include credentials depending on how
users invoke it. The exact risk is usage-dependent, but secrets in command-line
arguments and config files are common operational hazards.

**Why it matters:** Tooling around a database extension should not train users
to put passwords in shell history, process lists, or world-readable config.

**Impact:** Credential exposure in operational tooling.

**Fix direction:** Prefer `.pgpass`, libpq service files, env-only secrets, or
secret-manager integration in docs. Add a secret-scanning workflow such as
truffleHog or detect-secrets.

**Priority/window:** P2.

---

## 11. Ecosystem and Operational Readiness Gaps

### Finding O1 - No drain-mode end-to-end operational proof was found

**Severity:** Medium

**Evidence:** v0.36 changelog claims drain mode, and the repo has many
operational tests, but this audit did not find a focused E2E test that proves
maintenance drain semantics across pause, in-flight refresh completion, CDC
drain, and resume.

**Why it matters:** Drain mode is an operator-facing safety feature. Its value
depends on precise behavior under load.

**Impact:** Maintenance windows can still rely on manual sequencing.

**Fix direction:** Add a runbook plus E2E test: start long refresh, begin drain,
assert no new refresh dispatch, wait for active job completion, verify buffers,
resume, and verify catch-up.

**Priority/window:** P2.

### Finding O2 - Monitoring stack lacks enough alert-rule-as-code coverage

**Severity:** Medium

**Evidence:** [monitoring](../monitoring/) contains operational artifacts, and
the code emits many alert events via [src/monitor.rs](../src/monitor.rs), but
this audit did not find a complete, versioned alert rule set for freshness,
slot lag, Citus worker failures, EC-01 residual cleanup, history growth, and
trace export failures.

**Why it matters:** Production readiness is not only metrics existence; it is
also default actionable alerts.

**Impact:** Operators must compose their own alerting story from docs and SQL.

**Fix direction:** Ship Prometheus alert rules and Grafana panels for the core
SLOs: freshness, p99 refresh latency, CDC buffer depth, WAL slot lag, scheduler
worker saturation, Citus lease health, and failed refresh/error budget burn.

**Priority/window:** P2.

### Finding O3 - dbt, relay, and TUI should track new v0.36/v0.37 surface faster

**Severity:** Low/Medium

**Evidence:** The repo includes strong integration projects, but new core
features such as temporal IVM, vector aggregates, trace propagation, drain mode,
and force-full override need parallel UX in docs, dbt macros, relay/TUI status,
and examples.

**Why it matters:** Ecosystem value falls behind core capability if the extra
tools cannot observe or configure new states.

**Impact:** Users can create advanced stream tables but may not manage them well
through dbt or TUI workflows.

**Fix direction:** Add a release checklist item: every new SQL/GUC feature gets
SQL reference, configuration docs, TUI visibility if operational, dbt support
if modeling-related, and relay docs if event-related.

**Priority/window:** P3.

---

## 12. Missing Feature Opportunities

These are not defects, but they materially affect the "world-class" bar.

| Opportunity | Why it matters | Suggested release |
|---|---|---|
| `EXPLAIN STREAM TABLE` plan visualizer | Users need to know whether their query is DIFF-safe, FULL-fallback, join-heavy, or using vector/temporal operators. | v0.38/v0.39 |
| DVM generated-SQL artifact API | Debugging performance and correctness requires inspecting generated delta SQL without enabling broad logs. | v0.38 |
| Citus chaos harness | Distributed correctness cannot be inferred from single-node tests. | v0.38 |
| Durable CDC hold mode | Operators need to stop refresh pressure without dropping change capture. | v0.38 |
| Complete GUC/API generated catalog | 126 GUCs and 132 SQL definitions are too many for manual docs. | v0.38 |
| OpenTelemetry integration guide and collector test | Trace propagation needs operational proof. | v0.38 |
| SBOM and signed artifacts | Required for serious production adoption. | v1.0 prep |
| Per-query p99 and temp-spill regression gate | DVM performance must be tracked by query shape, not aggregate averages. | v0.39 |
| Row-id schema verifier in planner | Turns row-id design into a guardrail. | v0.38 |
| SQLancer light PR mode | A small oracle run on PRs could catch obvious DVM/FULL regressions before weekly runs. | v0.39 |

---

## 13. Prioritized Action Plan

| ID | Area | Action | Severity | Effort | Window |
|---|---|---|---|---|---|
| A01 | Correctness | Close EC-01 with unconditional PH-D1 mitigation plus principled row-id convergence fix. | Critical | L | v0.38 |
| A02 | Correctness | Wire `RowIdSchema::verify_pipeline()` into DVM planning. | High | M | v0.38 |
| A03 | CDC/WAL | Wire or deprecate `pg_trickle.enforce_backpressure`; add safe semantics and tests. | High | M | v0.38 |
| A04 | Scheduler | Fix event-driven wake docs/tests now; design latch-based wake separately. | High | S/M | v0.38 |
| A05 | Error handling | Construct `SpiErrorCode` from SPI errors on refresh/scheduler/catalog paths. | High | M | v0.38 |
| A06 | Citus | Add Citus E2E/chaos rig and workflow. | High | L | v0.38/v0.39 |
| A07 | Docs | Regenerate/update CONFIGURATION.md for all GUCs and correct event-driven wake. | High | S/M | v0.38 |
| A08 | Docs | Update UPGRADING.md through 0.37.0. | Medium | S | v0.38 |
| A09 | Ops | Split CDC pause into durable hold vs discard/reinit semantics. | High | M | v0.39 |
| A10 | Tests | Fix wake latency test so poll-only cannot pass event-driven assertions. | High | XS | v0.38 |
| A11 | Performance | Add TPC-H generated SQL + EXPLAIN artifacts for known collapse queries. | Medium | M | v0.39 |
| A12 | Config | Use or remove `history_prune_interval_seconds`. | Medium | XS | v0.38 |
| A13 | Security | Add SBOM, cosign signing, and provenance. | Medium | M | v1.0 prep |
| A14 | Fuzz | Add WAL decoder, merge codegen, DAG, and snapshot fuzz targets. | Medium | M/L | v0.39 |
| A15 | Inbox/outbox | Expand unit/property tests to envelope, dedup, leases, offsets. | Medium | M | v0.39 |
| A16 | Observability | Add OpenTelemetry guide and live collector E2E. | Medium | S/M | v0.38 |
| A17 | Maintainability | Continue splitting `api/mod.rs`, parser rewrites, and scheduler loop state. | Medium | L | ongoing |

---

## 14. Risk Register

| Risk | Severity | Likelihood | Current control | Residual concern |
|---|---|---:|---|---|
| Join delta phantom rows | Critical | Medium | Q15 allowlist, DVM tests, PH-D1 helper | Still not proven convergent; silent wrong results possible. |
| WAL slot lag fills disk | High | Medium | Lag metrics/warnings, inactive backpressure helper | Enforcement GUC not wired; operators may trust it. |
| Event-driven latency claim false | High | High | Warning at scheduler startup | Docs/tests still imply active low-latency wake. |
| SQLSTATE retry misclassification | High | Medium | SQLSTATE classifier exists | Real SPI errors still use text path. |
| Citus coordination failure | High | Medium | Unit-level/static code only | No chaos/topology E2E. |
| CDC pause drops changes | High | Low/Medium | Superuser-only GUC | Semantics can surprise incident responders. |
| History table growth | Medium | Medium | Daily batched prune | Interval GUC unused; daily may be too slow at high refresh rates. |
| Configuration doc drift | High | High | Manual docs | 126 GUCs make drift recurring. |
| Trace export failure | Medium | Medium | Integration tests for capture | No live collector or failure-mode tests. |
| Supply-chain trust gap | Medium | Medium | cargo-deny/security workflows | No SBOM/signing/provenance. |

---

## 15. Appendices

### Appendix A - Current Repository Metrics

| Metric | Value |
|---|---:|
| Current version | 0.37.0 |
| Rust files in `src/` | 78 |
| Rust LOC in `src/` | 118,860 |
| Rust files in `tests/` | 142 |
| Rust LOC in `tests/` | 90,841 |
| `GucRegistry::define*` calls | 126 |
| `#[pg_extern]` definitions | 132 |
| Non-archive SQL scripts | 40 |
| Archive SQL scripts | 80 |
| Workflow YAML files | 19 |
| Fuzz targets | 4 |
| Criterion benches | 3 |
| TODO/FIXME/HACK in `src/` | 0 |
| Approx. non-comment unsafe keyword lines | 364 |

### Appendix B - Test and CI Inventory

- Unit and pure Rust tests: `src/**` tests and [tests/property_tests.rs](../tests/property_tests.rs).
- Integration/Testcontainers tests: non-E2E files under [tests](../tests/), including
  [tests/integration_trace_tests.rs](../tests/integration_trace_tests.rs).
- E2E tests: 100+ `tests/e2e_*_tests.rs` files, including DVM, CDC, WAL, DAG,
  upgrade, pgvector, TPC-H, SQLancer, wake, schema evolution, and stability.
- Fuzz targets: [fuzz/fuzz_targets/cdc_fuzz.rs](../fuzz/fuzz_targets/cdc_fuzz.rs),
  [fuzz/fuzz_targets/cron_fuzz.rs](../fuzz/fuzz_targets/cron_fuzz.rs),
  [fuzz/fuzz_targets/guc_fuzz.rs](../fuzz/fuzz_targets/guc_fuzz.rs),
  [fuzz/fuzz_targets/parser_fuzz.rs](../fuzz/fuzz_targets/parser_fuzz.rs).
- Benchmark files: [benches/diff_operators.rs](../benches/diff_operators.rs),
  [benches/refresh_bench.rs](../benches/refresh_bench.rs),
  [benches/scheduler_bench.rs](../benches/scheduler_bench.rs).
- Primary CI: [.github/workflows/ci.yml](../.github/workflows/ci.yml).
- Long-running correctness/performance workflows:
  [.github/workflows/tpch-nightly.yml](../.github/workflows/tpch-nightly.yml),
  [.github/workflows/sqlancer.yml](../.github/workflows/sqlancer.yml),
  [.github/workflows/e2e-benchmarks.yml](../.github/workflows/e2e-benchmarks.yml),
  [.github/workflows/stability-tests.yml](../.github/workflows/stability-tests.yml).

### Appendix C - Prior-Assessment Status Summary

| Prior finding | Status in v8 |
|---|---|
| EC-01 join phantom rows | Still open, still critical. |
| Snapshot/restore atomicity | Fixed by `SnapSubTransaction` and explicit column lists. |
| SQLSTATE classifier | Partially fixed; classifier exists but is not wired into actual SPI error construction. |
| Citus chaos gap | Still open. |
| Inbox/outbox no unit tests | Partially fixed; helper tests exist but reliability contract needs tests. |
| L0 cache empty signal | Improved; process-local map exists, but not shared dshash. |
| History pruning | Improved; daily batched prune exists, but interval GUC is unused. |
| Force-full override | Fixed/implemented. |
| Scheduler/merge monoliths | Improved, not eliminated. |
| GUC documentation gap | Still open and larger due to v0.37 docs drift. |

### Appendix D - High-Value Verification Tasks

These are the fastest ways to turn the highest-risk hypotheses into hard facts:

1. Run EC-01 property stress with random co-deletes and scalar-subquery joins,
   comparing DIFF vs FULL after every cycle.
2. Enable `pg_trickle.enforce_backpressure`, create artificial slot lag, and
   prove whether source DML is paused or captured.
3. Run the wake test with `scheduler_interval_ms = 10000` and assert completion
   under 2 seconds; it should fail today because event-driven wake is disabled.
4. Search/replace SPI error construction through a single helper and run retry
   classifier tests by SQLSTATE class.
5. Build the smallest Citus topology test that verifies lease acquisition and
   worker-slot polling, then add worker kill/restart.
6. Generate a complete GUC list from [src/config.rs](../src/config.rs) and diff
   against [docs/CONFIGURATION.md](../docs/CONFIGURATION.md).

---

## Closing Assessment

pg_trickle is in a strong but delicate position. The codebase has the ambition,
test investment, and operational scope of a serious database subsystem. The
remaining gaps are not cosmetic: they are exactly the gaps that separate a
feature-rich extension from a platform operators can trust under failure,
locale variance, large joins, distributed topology changes, and incident
response.

The highest-leverage next step is a focused hardening milestone: close EC-01,
make the advertised safety GUCs truthful, correct the event-driven wake story,
add Citus chaos coverage, and regenerate public docs from source metadata. That
would move the project much closer to its stated world-class bar than another
round of new feature surface.