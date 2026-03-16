# Unit Test Suite Evaluation

> Date: 2026-03-16
> Scope: Rust unit tests under `src/`
> Method: source review of every unit-test file, compiled test inventory review, and a fresh `just test-unit` run

## Executive Summary

The unit test suite is currently green and substantial:

- `just test-unit` now passes with `1305 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`
- Unit tests now exist in `44` source files under `src/`
- `1` source file currently has no unit tests: `src/bin/pgrx_embed.rs`
- No unit tests are marked `#[ignore]` or `#[should_panic]`

## Implementation Status

The initial hardening slice from this report has been started and validated on branch `test-evals-unit-1`:

- Added direct unit coverage to `src/dvm/row_id.rs`
- Extracted and unit-tested pure worker-token helpers in `src/shmem.rs`
- Added direct normalization/helper tests in `src/config.rs`
- Extracted `_PG_init()` decision logic into a pure helper and tested it in `src/lib.rs`
- Added execution-backed integration tests for `semi_join` and `anti_join` that run the generated DVM SQL against a standalone PostgreSQL container on Linux/CI, covering initial match gain/loss transitions plus simultaneous left/right deltas and unmatched-left insert behavior; these tests are gated off on macOS because importing `pg_trickle` internals into an integration-test binary currently aborts with a pgrx flat-namespace symbol lookup failure
- Added Linux/CI-only execution-backed integration tests for `window` and `scalar_subquery`, covering partition-local `ROW_NUMBER` recomputation, frame-sensitive running `SUM(...) OVER (...)` recomputation, scalar-subquery inner-change fan-out, and outer-only passthrough behavior; subsequently extended with cross-partition UPDATE (partition-move recomputation of both affected partitions), simultaneous two-expression window recompute (`ROW_NUMBER` + running `SUM`), and simultaneous outer-and-inner change for the scalar-subquery DBSP `C₀` formula correctness
- Added Linux/CI-only execution-backed integration tests for representative aggregate families, covering grouped `COUNT(*)`, grouped `SUM`, grouped `AVG` rescan behavior, and a filtered grouped `COUNT(...)`
- Extended Linux/CI-only execution-backed aggregate coverage to rescan-heavy families, covering grouped `MIN`, grouped `MAX`, ordered `STRING_AGG`, and ordered-set `MODE()` recomputation
- Extended Linux/CI-only aggregate execution coverage again to object and percentile families, covering `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `PERCENTILE_CONT`, and `PERCENTILE_DISC`
- Added backend-backed parser summary coverage via `cargo pgrx test`, exercising real SQL parsing for representative CTE, window, scalar-subquery, and recursive-CTE queries

This closes the previously identified zero-coverage gap for `row_id.rs`, `shmem.rs`, `config.rs`, and `lib.rs`, extends the execution-backed hardening track across all four initially identified thin operators, broadens aggregate execution coverage across algebraic, extremum, object-aggregate, and ordered-set families, and adds the first backend-backed parser summary tests. Successful differential refresh behavior was already covered at the E2E layer by `tests/e2e_user_trigger_tests.rs`; the remaining highest-value work is deeper operator edge coverage, a narrower refresh-path seam if we decide it is worth the maintenance cost, and a macOS-compatible harness for the new DVM-internal integration tests.

## Remaining Work Summary

Still not started:

- A macOS-compatible harness for DVM-internal execution-backed integration tests

Started but still partial:

- Aggregate execution-backed coverage now includes grouped `COUNT(*)`, grouped `SUM`, grouped `AVG`, a filtered grouped `COUNT(...)`, grouped `MIN`, grouped `MAX`, ordered `STRING_AGG`, ordered-set `MODE()`, `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `PERCENTILE_CONT`, and `PERCENTILE_DISC`; remaining high-value cases are array-style JSON/JSONB aggregates, broader multi-group / mixed-family edge cases, and any aggregate families we decide are important enough to justify the harness cost beyond these representatives
- Thin-operator execution-backed coverage now exists for `semi_join`, `anti_join`, `window`, and `scalar_subquery`; `semi_join` / `anti_join` cover match-gain/loss, simultaneous left/right deltas, and unmatched-left inserts; `window` now covers partition-local recompute, frame-sensitive running SUM, cross-partition UPDATE (partition-move), and simultaneous two-expression recompute; `scalar_subquery` covers inner-change fan-out, outer-only passthrough, and simultaneous outer-and-inner change (DBSP `C₀` correctness); remaining highest-value thin-operator work is nested-source cases for semi/anti joins and aggregate-backed scalar-subquery cases
- Refresh-path coverage exists at the E2E layer (`tests/e2e_user_trigger_tests.rs` and related refresh suites), but there is still no narrower direct seam around `src/refresh.rs` itself if we decide that finer-grained coverage is worth the extra maintenance cost
- Parser integration summaries now exist for representative CTE, window, scalar-subquery, and recursive-CTE shapes via `cargo pgrx test`, but they are still a small summary slice rather than exhaustive SQL-shape coverage

Lower-priority follow-up:

- Scheduler lifecycle seams in `src/scheduler.rs`
- Trigger/runtime integration coverage in `src/cdc.rs`, `src/ivm.rs`, and shared-memory runtime coverage in `src/shmem.rs`
- Property/fuzz coverage for scanners, DAG invariants, and WAL/text parsers

My overall confidence in the unit suite is **moderate-high for pure Rust logic, but only moderate as a standalone signal for end-to-end correctness**.

That distinction matters:

- For pure helpers, enum/state logic, naming, hashing, schedule parsing, DAG algorithms, and many small SQL-fragment builders, the suite is strong.
- For generated differential SQL semantics, the suite is materially weaker than the raw test count suggests. Many tests verify that SQL contains expected fragments or comments, not that the SQL executes and produces the correct delta rows.
- For PostgreSQL-backend-bound logic, the unit suite is intentionally limited by test stubs around SPI, parse-tree walking, shared memory, triggers, and background workers.

## Bottom-Line Assessment

### What We Can Be Confident About

- The suite is excellent at catching regressions in **pure string/token processing**, **helper algorithms**, **enum/string conversions**, **graph algorithms**, and **metadata propagation**.
- The suite gives good coverage of the **shape** of DVM SQL generation: join parts, placeholder replacement, alias rewriting, row-id strategy selection, transition-table branching, and aggregate-family selection.
- The suite is broad enough that accidental renames, missing SQL fragments, aliasing regressions, or many class-of-bug mistakes will be caught quickly.

### What We Should Not Overclaim

- The unit suite does **not** by itself prove that generated differential SQL is semantically correct when run against PostgreSQL data.
- The unit suite does **not** meaningfully validate SPI-heavy code paths, background worker orchestration, shared-memory coordination, trigger installation/execution, or real raw-parser integration.
- Several thin operator files have only smoke-level structural assertions, which means the suite can miss real semantic bugs in the least-tested operators.

### Confidence Rating

| Dimension | Rating | Notes |
|---|---|---|
| Suite health | High | Green, fast, broad, no ignored tests |
| Pure helper logic | High | Strong coverage in `api.rs`, `dag.rs`, `error.rs`, `version.rs`, `wal_decoder.rs` |
| SQL-template shape coverage | Moderate-high | Many DVM operators are checked for structure, aliases, fragments, and regression markers |
| Semantic correctness of generated SQL | Moderate-low | Too many tests stop at `contains(...)` rather than executing SQL |
| PostgreSQL backend boundary coverage | Low-moderate | Parser/SPI/shared-memory/trigger paths are mostly out of reach in unit tests |
| Overall unit-suite confidence | Moderate-high | Good for fast regression detection, insufficient alone for semantic guarantees |

## Method Notes

The suite is too large for a useful one-line comment on all `1284` tests individually. For maintainability, this report groups dense files into coherent named-test families where dozens of tests exercise the same helper pattern. Sparse files are assessed effectively test-by-test.

That is the right granularity here: the goal is not to restate every function name, but to answer whether the suite actually proves the behavior it claims to cover.

## Inventory Snapshot

### Highest-Density Files

| File | Tests | Primary theme |
|---|---:|---|
| `src/dvm/parser.rs` | 355 | expression rendering, operator metadata, IVM support classification, aliasing, recursive/window/CTE metadata |
| `src/dvm/operators/aggregate.rs` | 116 | aggregate eligibility, delta/merge SQL generation, filter/rescan behavior |
| `src/api.rs` | 103 | schedule parsing, SQL token scanning, query-rewrite helpers, config diff |
| `src/dvm/operators/recursive_cte.rs` | 77 | recursive CTE SQL generation and self-reference analysis |
| `src/dag.rs` | 77 | topological ordering, cycles, diamonds, SCCs, execution units |
| `src/refresh.rs` | 49 | refresh-action selection, frontier placeholders, caches, adaptive thresholds |
| `src/dvm/mod.rs` | 42 | top-level query splitting, cache helpers, scan-chain classification |
| `src/dvm/diff.rs` | 34 | diff context, quoting, CTE building, dispatcher plumbing |
| `src/wal_decoder.rs` | 33 | naming, decoded-output parsing, schema mismatch detection |
| `src/dvm/operators/scan.rs` | 33 | scan delta SQL, transition-table mode, keyless net counting |

### Files With No Unit Tests

| File | Risk | Notes |
|---|---|---|
| `src/bin/pgrx_embed.rs` | Low | Probably low-value generated/tooling path |

## Per-File Assessment

### High-Confidence Files

| File | Tests | What the tests do | Does the suite actually prove it? | Mitigations |
|---|---:|---|---|---|
| `src/api.rs` | 103 | Covers `inject_pgt_count`, DISTINCT stripping, comma splitting, keyword detection, cron parsing/validation, `cron_is_due`, `detect_select_star`, CDC/refresh-mode interaction, whitespace normalization, and config diffing. | **Mostly yes** for pure helper logic. The assertions are direct and specific. The main gap is that SPI/GUC-backed schedule validation and SQL-callable API workflows are not exercised. | Add unit seams for GUC-backed duration schedule validation. Add property tests for token scanners (`find_top_level_keyword`, comma splitting, `detect_select_star`). Keep backend-facing API behavior in integration/E2E. |
| `src/error.rs` | 11 | Verifies error classification, retryability, suspension accounting, SPI error retry heuristics, retry policy backoff, and retry-state lifecycle. | **Yes.** These are pure decision tables and state transitions; unit tests are the right tool and current assertions are strong. | Add property checks for monotonic backoff and max-attempt invariants. |
| `src/hash.rs` | 7 | Validates determinism, distinct inputs, null-marker behavior, separator collision prevention, empty-string handling, and `u64 -> i64` casting safety. | **Mostly yes.** This is pure hashing logic and the tests directly assert intended invariants. | Add direct tests of `pg_trickle_hash()` / `pg_trickle_hash_multi()` wrappers rather than only underlying `xxh64`. |
| `src/dag.rs` | 77 | Covers topological sort, cycle detection, schedule resolution, diamonds, consistency groups, execution-unit DAG building, SCCs, condensation order, and topological levels. | **Yes for the pure graph algorithms.** This is one of the best parts of the suite. It checks structure, order, and many edge cases, including diamonds and overlapping groups. | Add property-based DAG generation to cross-check topological and SCC invariants. Add a few tests around pathological large graphs and repeated edges. |
| `src/version.rs` | 19 | Tests canonical period selection, frontier storage/merge, LSN comparisons, serialization, and target timestamp selection. | **Yes.** Direct, deterministic value assertions. | Add property tests for frontier merge associativity and idempotence. |
| `src/wal_decoder.rs` | 33 | Covers slot/publication naming, quoted identifiers, action detection, column extraction, PK hash construction, schema-mismatch detection, and old-key parsing. | **Mostly yes** for the current string-based decoder helpers. The tests are direct and useful. What they do not prove is correctness against real replication output across PostgreSQL versions. | Add fixtures from real `pgoutput` logs captured from integration tests. Add malformed-input fuzz/property tests. |
| `src/monitor.rs` | 19 | Covers alert event/value formatting, payload escaping/truncation, CDC health alert detail text, and dependency-tree rendering. | **Mostly yes.** Pure rendering logic is well suited to unit tests. | Add threshold-boundary tests that cross-check alert-severity transitions with scheduler/integration behavior. |
| `src/bin/pg_trickle_dump.rs` | 4 | Tests topo ordering, restore SQL handling of non-active statuses, dollar-quote selection, and qualified-name quoting. | **Mostly yes** for helper routines. | Add a golden-file style test for a realistic multi-ST dump/restore sequence. |

### Broad But Only Partially Semantic Files

| File | Tests | What the tests do | Does the suite actually prove it? | Mitigations |
|---|---:|---|---|---|
| `src/dvm/parser.rs` | 355 | Very broad coverage of `Expr` rendering, output names, alias rewriting, monotonicity, `OpTree` metadata, source OIDs, row-id/key-column heuristics, aggregate classification, HAVING rewrites, CTE metadata, recursive CTE metadata, window metadata, and IVM support classification. | **Partially.** This file is broad, but many tests exercise the model objects directly, not PostgreSQL parse-tree walking. Crucially, `parse_query` and `parse_first_select` are test stubs in unit mode, so the unit suite does not prove actual SQL-to-`OpTree` parsing. | Add parser-focused integration tests that compare real SQL inputs to expected `OpTree` summaries. Add golden tests at the SQL boundary. Keep unit tests for model logic, but stop treating them as parser-end-to-end proof. |
| `src/dvm/operators/aggregate.rs` | 116 | Covers direct-aggregate eligibility, aggregate delta expressions, merge expressions, filter handling, rescan SQL rendering, many aggregate families, MIN/MAX logic, JSON/JSONB/ordered-set/user-defined handling, and generated SQL markers in `diff_aggregate`. | **Partially.** The breadth is excellent, but most tests assert SQL fragments such as `LEAST`, `GREATEST`, `FILTER`, `IS DISTINCT FROM`, `LATERAL`, or rescan SQL text. They do not execute the generated SQL or compare results under inserts/deletes. | Initial execution-backed coverage now spans `COUNT`, `SUM`, `AVG`, filtered `COUNT`, `MIN`, `MAX`, `STRING_AGG`, `MODE()`, `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `PERCENTILE_CONT`, and `PERCENTILE_DISC`. Remaining work is broader multi-group / mixed-family cases and any additional aggregate families we decide are worth testing beyond those representatives. |
| `src/dvm/operators/recursive_cte.rs` | 77 | Covers self-reference counting, alias collection, seed/cascade/query SQL generation, targeted recomputation SQL, nonlinear seed generation, and many error paths. | **Partially.** This is a strong generator suite, but it mostly proves that expected SQL fragments are emitted and that unsupported patterns error out. It does not prove fixpoint semantics. | Add execution-backed tests for one linear recursive CTE, one nonlinear case, one filtered recursive case, and one project-over-join case. |
| `src/dvm/mod.rs` | 42 | Covers top-level `UNION ALL` / set-op splitting, delta template substitution, scan-chain classification, cache operations, `needs_pgt_count`, and scalar-aggregate root detection. | **Mostly yes** for string decomposition and cache state. Still, query splitting helpers are brittle enough that property/fuzz testing would add value. | Add property tests for nested parentheses, quoted strings, comments, and mixed-case keywords. |
| `src/dvm/diff.rs` | 34 | Covers quoting helpers, column list building, `DiffContext` defaults, placeholder handling, CTE naming/building, recursive CTE registration, delta cache operations, dispatcher plumbing, and simple `differentiate()` end-to-end shape. | **Partially.** Good for plumbing, not enough for semantic proof. The end-to-end tests only assert that generated SQL contains expected scaffolding. | Add execution-backed tests for `differentiate()` on a few representative trees and cache invalidation tests across changed defining queries. |
| `src/dvm/operators/scan.rs` | 33 | Covers change-buffer references, placeholder vs literal LSNs, PK/hash selection, typed column refs, delete/insert branches, merge-safe dedup, transition-table mode, and keyless net-counting structure. | **Partially.** Strong generator coverage, especially for transition tables and keyless paths, but still predominantly substring-based. | Execute representative scan deltas against seeded change-buffer rows, especially keyless net-counting and transition-table update cases. |
| `src/dvm/operators/join.rs` | 33 | Covers inner join SQL structure, nested joins, pre-change snapshots (`L0`, `R0`), semijoin-aware behavior, natural joins, equijoin key extraction, and correction-path regressions. | **Partially.** The suite is valuable and dense, but it proves structure and intended code path selection, not result correctness. | Add execution-backed checks for a basic inner join, a three-table join chain, a natural join, and a correction-path regression fixture. |
| `src/dvm/operators/join_common.rs` | 20 | Covers snapshot SQL generation, source-alias detection, simple-child/source classification, join-condition rewriting, and key-expression fallback logic. | **Mostly yes** for helper logic. It still benefits from property-style stress around quoted/raw alias rewriting. | Add table-driven tests for more raw SQL rewrite edge cases and collisions between alias names. |
| `src/refresh.rs` | 49 | Covers refresh-action selection, early rejection in `execute_differential_refresh`, LSN placeholder resolution, merge-template cache behavior, SQL parameterization helpers, adaptive thresholds, and append-only MERGE rewriting. | **Partially.** The file is useful, but the biggest semantic gap is obvious: `execute_differential_refresh` is only tested for rejection paths, not for a successful differential refresh. | Add a success-path unit seam or integration test for `execute_differential_refresh`. Add execution-backed tests for append-only SQL rewriting and prepared-statement parameter ordering. |
| `src/hooks.rs` | 23 | Covers schema-change kind comparisons, function-name extraction, DDL event classification, and snapshot-vs-current column comparison. | **Mostly yes** for pure classification helpers. It does not test actual event-trigger integration, dropped-object traversal, or SPI catalog lookups. | Add integration tests that fire real DDL and verify classification/reinitialize/block decisions. |
| `src/cdc.rs` | 23 | Covers trigger naming, PK-hash trigger expressions, changed-column bitmask generation, partition-bound parsing, and typed column-definition rendering. | **Mostly yes** for string builders. It does not prove trigger function correctness or DDL installation behavior. | Add integration tests that install triggers and verify emitted change-buffer rows, especially keyless and wide-row cases. |
| `src/ivm.rs` | 26 | Covers simple-scan-chain detection, keyed/keyless delete/insert SQL generation, column list building, and trigger name generation. | **Partially.** It proves helper and SQL-builder structure, not actual trigger semantics or duplicate-preserving behavior. | Add execution-backed tests for keyed and keyless DELETE/INSERT SQL and one trigger-fire integration test. |
| `src/scheduler.rs` | 28 | Covers time helpers, `RefreshOutcome`, due-policy logic, lag detection, worker-extra parsing, and some state-struct invariants. | **Partially.** Helpful for pure decisions, but it barely touches real scheduler behavior. There is no meaningful unit coverage for enqueue/claim/complete/cancel, database dispatch, or worker recovery. | Introduce a storage abstraction or fake repository to unit-test scheduler job lifecycle. Add tests for crash recovery and wave-reset logic against realistic state transitions. |

### Medium-Confidence Operator Files

| File | Tests | What the tests do | Does the suite actually prove it? | Mitigations |
|---|---:|---|---|---|
| `src/dvm/operators/except.rs` | 16 | Covers set/all semantics, non-commutativity, count math, boundary handling, row IDs, dual counts, storage-table join, and wrong-node errors. | **Partially.** Stronger than most thin operators, but still structural. | Execute `EXCEPT` / `EXCEPT ALL` cases with duplicates and invisible-row transitions. |
| `src/dvm/operators/intersect.rs` | 14 | Covers set/all semantics, boundary crossings, branch tagging, delete zeroing, row IDs, dual counts, count aggregation, and storage-table join. | **Partially.** Same pattern as `except.rs`: good structure coverage, limited semantic proof. | Add result-execution fixtures with duplicates and mixed insert/delete cycles. |
| `src/dvm/operators/outer_join.rs` | 12 | Covers left-join parts, `R0` reconstruction, insert/delete partitioning, null padding, delta flags, nesting, natural joins, and wrong-node errors. | **Partially.** Valuable regression markers, but no execution against real row changes. | Add execution-backed left-join cases: unmatched-to-matched, matched-to-unmatched, and nested left join. |
| `src/dvm/operators/full_join.rs` | 9 | Covers full-join part structure, `R0` via `EXCEPT ALL`, null padding, delta flags, nesting, and wrong-node errors. | **Partially.** Good structural coverage, weak semantic proof. | Add execution-backed full-join transitions for both-sided unmatched/matched flips. |
| `src/dvm/operators/filter.rs` | 9 | Covers basic filtering, predicate inclusion, row-id/action passthrough, dedup propagation, and raw SQL column-ref rewriting. | **Mostly yes** for helper behavior. | Add property tests for raw predicate rewriting and a few executed filter delta cases. |
| `src/dvm/operators/project.rs` | 10 | Covers alias renaming, row-id passthrough, dedup propagation, and expression resolution. | **Mostly yes** for pure transformations. | Add more expression-shape coverage for nested raw expressions and alias collisions. |
| `src/dvm/operators/lateral_function.rs` | 20 | Covers output-column inference, ordinality, old-row re-expansion, insert-only expansion, alias handling, and inferred defaults for `jsonb_each` / `jsonb_array_elements`. | **Partially.** Better than a smoke suite, but still mostly SQL-shape assertions. | Add executed fixtures for `jsonb_each`, `jsonb_array_elements`, `WITH ORDINALITY`, and duplicate left-row updates. |
| `src/dvm/operators/lateral_subquery.rs` | 18 | Covers lateral keyword usage, left-join mode, null-safe hash behavior, old-row join conditions, alias/original alias handling, and output-column inference. | **Partially.** Similar to lateral function coverage: good structure, limited semantics. | Add execution-backed tests with correlated subquery changes and null-producing left joins. |
| `src/catalog.rs` | 14 | Covers `CdcMode`/`JobStatus` conversion, display, equality, terminal-state logic, and roundtrips. | **Yes** for those enums, but that is a narrow slice of the module's real behavior. | Add unit seams for pure catalog helper logic if more of `catalog.rs` becomes testable; otherwise rely on integration tests for CRUD/SPI paths. |

### Thin or Low-Confidence Files

| File | Tests | What the tests do | Does the suite actually prove it? | Mitigations |
|---|---:|---|---|---|
| `src/dvm/operators/semi_join.rs` | 3 | `test_diff_semi_join_basic`, `test_diff_semi_join_sql_contains_exists`, and wrong-node error. | **Only weakly in unit scope.** The source-file tests still prove only output columns and SQL shape, but they are now supplemented by Linux/CI-only execution-backed coverage for match-gain/loss transitions, simultaneous left/right deltas, and unmatched-left inserts. | Remaining work is narrower: nested semijoin sources, correlated right-delta edge cases, and a macOS-compatible local harness. |
| `src/dvm/operators/anti_join.rs` | 3 | Basic output-column check, SQL contains `NOT EXISTS`, and wrong-node error. | **Only weakly in unit scope.** The source-file tests still prove mostly structure, but they are now supplemented by Linux/CI-only execution-backed coverage for regain/loss transitions, simultaneous left/right deltas, and unmatched-left inserts. | Remaining work is narrower: nested anti-join sources, null/absence edge cases, and a macOS-compatible local harness. |
| `src/dvm/operators/window.rs` | 5 | Basic window SQL shape, changed-partition detection, unpartitioned full recompute marker, dedup flag, and wrong-node error. | **Weakly in unit scope, but now supplemented by Linux/CI-only execution-backed coverage.** Executed tests confirm partition-local `ROW_NUMBER` recompute, frame-sensitive running `SUM`, cross-partition UPDATE (partition-move), and simultaneous two-expression recompute. Remaining gaps: unpartitioned full-recompute and Window-over-Aggregate ordering. | Remaining: unpartitioned case; Window-over-Aggregate column-ordering test; macOS-compatible harness. |
| `src/dvm/operators/union_all.rs` | 5 | Two-child and three-child structure, empty-child error, dedup flag, wrong-node error. | **Weakly.** It checks scaffolding only. | Add execution-backed tests for duplicate preservation and row-id uniqueness across branches. |
| `src/dvm/operators/distinct.rs` | 5 | Basic boundary-crossing SQL, row-id hashing, dedup flag, and wrong-node error. | **Weakly to moderately.** Better than union/window because it checks boundary formulas, but still not executed. | Execute duplicate appear/disappear cases and mixed insert/delete cycles. |
| `src/dvm/operators/cte_scan.rs` | 6 | Basic body reuse, caching, alias application, missing-CTE error, wrong-node error. | **Moderately.** This is mostly wrapper logic, so unit tests help, but they do not stress multi-reference invalidation or recursive interactions. | Add tests for registry invalidation and cross-reference with changed body schemas. |
| `src/dvm/operators/subquery.rs` | 4 | Transparent passthrough, alias-renaming wrapper CTE, dedup preservation, wrong-node error. | **Mostly yes** for the tiny helper surface. | Add one executed nested-subquery case to prove wrapper semantics. |
| `src/dvm/operators/scalar_subquery.rs` | 4 | Basic structure, Part 1/Part 2 markers, `EXCEPT ALL` pre-change snapshot, wrong-node error. | **Weakly in unit scope, but now supplemented by Linux/CI-only execution-backed coverage.** Executed tests cover inner-change fan-out, outer-only passthrough, and simultaneous outer-and-inner change (validates DBSP `C₀` pre-image correctness). Remaining gap: shared-source overlap where outer and inner use the same source OID. | Remaining: shared-source (same OID for outer and inner); aggregate-backed inner subquery; macOS-compatible harness. |
| `src/dvm/operators/test_helpers.rs` | 2 | Helper sanity only. | **Minimal value by itself.** | Fine as-is, but do not count it as meaningful coverage. |

## Cross-Cutting Findings

### 1. Test count is high, but semantic execution is much lower than it looks

The biggest risk in the current suite is counting SQL-template assertions as if they were semantic correctness tests. A representative pattern looks like this:

- build a synthetic `OpTree`
- call `diff_*`
- render SQL
- assert `sql.contains("EXCEPT ALL")`, `sql.contains("Part 1")`, `sql.contains("LATERAL")`, or `sql.contains("IS DISTINCT FROM")`

That is useful, but it only proves that the code chose a branch or emitted a fragment. It does **not** prove that the resulting query computes the correct delta rows under realistic inserts, deletes, updates, duplicates, nulls, or mixed-source changes.

This is most acute in:

- `src/dvm/operators/aggregate.rs`
- `src/dvm/operators/join.rs`
- `src/dvm/operators/outer_join.rs`
- `src/dvm/operators/full_join.rs`
- `src/dvm/operators/window.rs`
- `src/dvm/operators/semi_join.rs`
- `src/dvm/operators/anti_join.rs`
- `src/refresh.rs`

### 2. Parser unit tests are broad but not end-to-end

`src/dvm/parser.rs` has the highest count in the suite and is clearly maintained carefully. That is good. But unit mode stubs out actual PostgreSQL parsing entry points. The result is:

- strong confidence in `Expr`, `OpTree`, aliasing, metadata, and support-classification helpers
- materially lower confidence in actual SQL-to-operator-tree conversion

This file should be treated as **high-value model coverage**, not as proof that real SQL parsing is covered.

### 3. Backend-bound orchestration code is still under-covered

The suite is weakest where correctness depends on PostgreSQL runtime services:

- SPI work
- background workers
- shared memory
- real event triggers
- real row/statement triggers
- prepared statements and MERGE execution

This is visible in:

- `src/refresh.rs`: no success-path differential refresh test
- `src/scheduler.rs`: no realistic job lifecycle test
- `src/shmem.rs`: only the extracted pure token/accounting helpers are covered; shared-memory integration itself is still untested
- `src/lib.rs`: `_PG_init()` decision branching now has direct unit coverage, but runtime registration side effects remain integration-only
- `src/cdc.rs` / `src/ivm.rs`: no actual trigger execution tests

### 4. Thin operators are the easiest place for subtle bugs to survive

The least-tested operators are not necessarily the simplest ones. `SEMI JOIN`, `ANTI JOIN`, `WINDOW`, and `SCALAR SUBQUERY` have tricky semantics but thin unit coverage. Initial execution-backed coverage now exists for all four, but the remaining scenarios in those operators are still high-value because most of the current operator suites remain structural rather than result-level.

### 5. Untested small modules still matter

`src/dvm/row_id.rs` is small, but row-id strategy mistakes can create correctness failures that are hard to debug. `src/shmem.rs` is more serious: if worker-token or generation bookkeeping is wrong, the scheduler can wedge, over-dispatch, or fail to invalidate caches.

## Priority Mitigations

### Priority 0: Highest-value hardening

1. Add a success-path test for `execute_differential_refresh()`.
2. Extend aggregate execution-backed coverage from the current `COUNT(*)` / `SUM` / `AVG` / filtered `COUNT(...)` / `MIN` / `MAX` / `STRING_AGG` / `MODE()` / `JSON_OBJECT_AGG` / `JSONB_OBJECT_AGG` / `PERCENTILE_CONT` / `PERCENTILE_DISC` slice into broader multi-group and mixed-family edge cases as needed.
3. Add direct unit coverage for `src/dvm/row_id.rs` and `src/shmem.rs`. Completed in the initial hardening slice.
4. Add parser integration tests that validate real SQL-to-`OpTree` summaries, since unit tests cannot prove that today.

### Priority 1: Reduce false confidence from SQL-fragment tests

1. For each major DVM operator, keep one structural SQL test but add at least one result-level execution test.
2. Prefer assertions against exact normalized SQL or result rows over `contains(...)` when practical.
3. For fragile generators, use golden SQL fixtures only when the SQL text itself is the contract; otherwise execute the SQL.

### Priority 2: Expand property/fuzz style coverage

1. Add property tests for top-level SQL token scanners in `api.rs` and `dvm/mod.rs`.
2. Add randomized DAG tests for `dag.rs` invariants.
3. Add malformed-input fuzz cases for decoder/text parsers in `wal_decoder.rs`.

### Priority 3: Cover currently untested files

1. `src/dvm/row_id.rs`: test each enum variant and any strategy-selection helper added around it.
2. `src/shmem.rs`: extract pure compare-and-swap logic behind a trait or helper and test token-acquire/release/reconcile invariants. Completed for the pure helper layer; shared-memory runtime integration still needs higher-tier coverage.
3. `src/config.rs`: add table-driven tests around string-to-mode parsing and default values if exposed by helper functions. Started with direct normalization/threshold helper tests.
4. `src/lib.rs`: extract preload decision logic into a pure helper so `_PG_init()` behavior can be unit tested. Completed.

## Recommended Hardening Backlog By File

| Priority | File | Suggested additions |
|---|---|---|
| P0 | `src/dvm/operators/semi_join.rs` | Linux/CI-only result-level tests now cover match gain/loss, simultaneous left/right deltas, and unmatched-left inserts. Remaining work: nested source case, correlated-right edge cases, and a macOS-compatible local harness. |
| P0 | `src/dvm/operators/anti_join.rs` | Linux/CI-only result-level tests now cover regain/loss, simultaneous left/right deltas, and unmatched-left inserts. Remaining work: null/absence transitions, nested source case, and a macOS-compatible local harness. |
| P0 | `src/dvm/operators/window.rs` | Linux/CI-only result-level tests now cover partition-local `ROW_NUMBER` recompute, frame-sensitive running `SUM`, cross-partition UPDATE (partition-move that recomputes both old and new partitions), and simultaneous two-expression recompute (`ROW_NUMBER` + running `SUM` together). Remaining work: unpartitioned full-recompute case, Window-over-Aggregate ordering, and a macOS-compatible local harness. |
| P0 | `src/dvm/operators/scalar_subquery.rs` | Linux/CI-only result-level tests now cover inner-change fan-out, outer-only passthrough, and simultaneous outer-and-inner change (validates DBSP `C₀` pre-image formula that excludes newly inserted outer rows from Part-2 DELETE output). Remaining work: shared-source overlap case (outer and inner share same source OID), aggregate-backed scalar subquery, and a macOS-compatible local harness. |
| P0 | `src/refresh.rs` | Success-path differential refresh test; prepared statement parameter-order test |
| P0 | `src/dvm/parser.rs` | SQL-to-tree integration summary tests using real PostgreSQL parsing |
| P0 | `src/dvm/operators/aggregate.rs` | Initial Linux/CI-only result-level tests now cover grouped `COUNT(*)`, grouped `SUM`, grouped `AVG`, filtered grouped `COUNT(...)`, grouped `MIN`, grouped `MAX`, ordered `STRING_AGG`, ordered-set `MODE()`, `JSON_OBJECT_AGG`, `JSONB_OBJECT_AGG`, `PERCENTILE_CONT`, and `PERCENTILE_DISC`. Remaining work: broader multi-group / mixed-family edge cases and any extra aggregate families we decide are important enough to execute directly. |
| P1 | `src/shmem.rs` | Pure helper extraction + worker token/accounting tests. Initial pure-helper coverage completed; integration coverage still pending. |
| P1 | `src/dvm/row_id.rs` | Direct unit tests for strategy enum and selection rules. Initial direct coverage completed. |
| P1 | `src/scheduler.rs` | Fake-repository tests for enqueue/claim/complete/retry/cancel |
| P1 | `src/cdc.rs` | Integration tests for trigger-generated rows, keyless and wide-row cases |
| P1 | `src/ivm.rs` | Executed keyed/keyless DML SQL behavior tests |
| P1 | `src/config.rs` | Direct normalization/default-value tests. Initial helper coverage completed; broader accessor/default coverage remains optional. |
| P1 | `src/lib.rs` | `_PG_init()` preload/warning decision helper tests. Initial coverage completed. |
| P2 | `src/api.rs` | Property tests for SQL scanners and duration/cron boundary fuzzing |
| P2 | `src/dvm/mod.rs` | Fuzz/property tests for set-op splitters and quoted-string nesting |
| P2 | `src/wal_decoder.rs` | Decoder fuzzing and real fixture corpus |
| P2 | `src/dag.rs` | Random DAG invariant checks |

## Suggested Confidence Statement For Planning Purposes

If I had to summarize the current state in one sentence:

> The unit suite is strong enough to catch a large share of fast-moving logic regressions, but not strong enough to independently justify confidence in DVM semantic correctness or PostgreSQL-runtime integration.

That means we should trust the unit suite as:

- a fast regression net
- a strong guardrail for helper logic
- a good design-pressure signal for pure code

We should **not** trust it as the primary proof layer for:

- generated SQL correctness
- parser correctness from real SQL
- trigger/runtime behavior
- scheduler/shared-memory coordination

## Recommended Next Actions

1. Add a success-path `execute_differential_refresh` test.
2. Add parser integration summary tests so `parser.rs` coverage matches the apparent confidence implied by its test count.
3. Extend aggregate execution-backed coverage into the remaining rescan and ordered-set families, and deepen thin-operator edge cases.
4. Add a fake-repository or similar seam for higher-value `scheduler.rs` lifecycle tests.
