# pg_trickle Overall Assessment — Deep Gap Analysis

Status: drafted from static analysis and targeted source inspection on the current workspace.
Scope: PostgreSQL 18 / pgrx 0.18.x extension, Rust source, SQL migrations, docs, tests, CI, Docker, CNPG, and monitoring assets.

## Executive Summary

`pg_trickle` has moved well beyond a prototype. The current tree contains substantial correctness hardening: keyless duplicate handling is implemented with net-counting and explicit DML paths, manual refresh reloads metadata after both advisory and row locks, unsafe blocks are generally documented, SQL SECURITY DEFINER functions mostly pin `search_path`, and the test suite covers a very large surface of joins, aggregates, WAL CDC, keyless tables, DAG scheduling, upgrade paths, and operational behavior.

The main risk is not a single obvious missing subsystem. The main risk is drift: the DVM engine is complex enough that small cache-key, placeholder, fallback, and concurrency mistakes can silently produce wrong rows; documentation and generated catalogs no longer match the implementation; and CI gates do not run the same expensive paths that the project advertises as release-critical. This makes the project look more complete than it is for users and reviewers.

Highest-priority issues:

| Priority | Finding | Why it matters |
| --- | --- | --- |
| P0 | DVM snapshot CTE cache keys only use leaf aliases | Can reuse a snapshot for a structurally different subtree with the same leaves, risking wrong deltas in complex joins. |
| P0 | LSN/template placeholder resolvers do not assert full resolution | A newly introduced or missed placeholder can reach execution as malformed SQL or, worse, use an unintended frontier. |
| P0 | WAL CDC transition lacks a final eligibility recheck at commit point | Replica identity/table shape can change between eligibility and transition, risking invalid WAL decoding assumptions. |
| P1 | `repair_stream_table` is documented in multiple places but not implemented | Backup/restore guidance tells users to run a function that does not exist. |
| P1 | SQL/GUC generated catalogs are broken | Public API and configuration docs omit many SQL functions and show registration placeholders for GUCs. |
| P1 | CI does not continuously exercise full E2E, E2E coverage, fuzz targets, or real Windows tests | Regressions can land in exactly the paths that carry the highest correctness risk. |
| P1 | Documentation still references stale GUC/function names and deprecated/no-effect settings | Operators can tune the wrong knobs and build bad runbooks. |

Important corrections to avoid stale findings:

| Earlier suspicion | Current assessment |
| --- | --- |
| Keyless duplicate rows are still an active critical data-loss bug | Not supported by current code. `src/dvm/operators/scan.rs`, `src/refresh/merge/mod.rs`, `src/api/mod.rs`, and `tests/e2e_keyless_duplicate_tests.rs` show EC-06-style handling is implemented and tested. There are stale comments, but not an obvious active duplicate-loss bug. |
| Manual refresh uses stale metadata after acquiring locks | Not supported by current code. `refresh_stream_table_impl` reloads `StreamTableMeta::get_by_name` after both the advisory lock and row lock. |
| SECURITY DEFINER functions broadly lack `search_path` hardening | Mostly false. Relay and CDC functions set explicit search paths. The IVM trigger path is the exception because it includes `public`. |
| Production Rust is full of direct `unwrap()` / `panic!()` | Targeted scans did not find active non-test panic/unwrap patterns in the main `src/` paths that were searched. Some background worker paths intentionally use `unwrap_or` fallbacks, which is a different reliability issue. |

## Methodology

The assessment combined broad repository inventory with targeted validation of high-risk claims.

Reviewed areas:

| Area | Representative files |
| --- | --- |
| SQL APIs and lifecycle | `src/api/mod.rs`, `src/api/helpers.rs`, `src/api/diagnostics.rs`, `src/catalog.rs` |
| DVM parser and operators | `src/dvm/mod.rs`, `src/dvm/diff.rs`, `src/dvm/parser/*`, `src/dvm/operators/*` |
| Refresh and merge engine | `src/refresh/codegen.rs`, `src/refresh/merge/mod.rs`, `src/refresh/phd1.rs` |
| CDC/WAL | `src/cdc.rs`, `src/wal_decoder.rs`, `src/ivm.rs` |
| Scheduling/scalability | `src/scheduler/mod.rs`, `src/scheduler/pool.rs`, `src/scheduler/cost.rs`, `src/dag.rs`, `src/shmem.rs` |
| Configuration and docs | `src/config.rs`, `scripts/gen_catalogs.py`, `docs/*.md`, `README.md`, `INSTALL.md`, `AGENTS.md` |
| Tests and CI | `tests/**/*.rs`, `fuzz/*`, `benches/*`, `.github/workflows/*`, `justfile` |
| Packaging/ops | `Dockerfile.*`, `tests/Dockerfile.*`, `monitoring/*`, `cnpg/*`, `sql/*.sql` |

Commands and checks used included file inventory, regex searches for panic/unwrap, SECURITY DEFINER/search_path, deprecated GUCs, sleeps in tests, workflow gates, WAL transition code, generated docs drift, and direct source reads around every high-severity finding.

Limitations:

| Limitation | Effect |
| --- | --- |
| No full test suite was run while drafting this report | Findings are source-audit findings, not post-test failures. |
| No live PostgreSQL cluster was exercised for this assessment | Concurrency and WAL transition findings are based on code paths and missing targeted tests. |
| Some findings are risk findings rather than proven failing repros | Recommended fixes include adding repro tests before changing logic. |

## Dimension 1 - Correctness

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| COR-01 | High | `src/dvm/diff.rs` (`snapshot_cache_key`, `get_or_register_snapshot_cte`) | Snapshot CTE cache keys are derived from sorted leaf aliases joined with `+`. That key ignores join predicates, join type, subtree shape, filters, projections, and aliases above the leaves. | Two different subplans over the same leaves can share a snapshot CTE. In complex join trees, lateral rewrites, nested joins, or different predicates over the same base aliases, a delta branch can read the wrong snapshot and produce incorrect inserts/deletes. | Replace the key with a structural fingerprint of the `OpTree`: operator type, join type, predicates, projected columns, filters, grouping, and child fingerprints. Add unit tests with two subtrees using identical leaf aliases but different join predicates/shapes. |
| COR-02 | High | `src/dvm/mod.rs` (`resolve_delta_template`), `src/refresh/codegen.rs` (`resolve_lsn_placeholders`) | Placeholder resolution substitutes known `__PGS_PREV_LSN_*__`, `__PGS_NEW_LSN_*__`, and pgt-prefixed tokens but does not visibly validate that all placeholders are gone. | A missed source OID, new placeholder family, or malformed cached template can survive until execution. That turns a deterministic template-generation bug into late SQL failure or incorrect frontier use. | After substitution, run a strict unresolved-token check such as `__PGS_[A-Z0-9_]+__|__PGT_[A-Z0-9_]+__`; return a typed error with the stream table and unresolved token. Add unit tests for known, unknown, and mixed placeholder sets. |
| COR-03 | High | `src/wal_decoder.rs` (`check_transition_eligible`, `finish_wal_transition`) | WAL transition eligibility checks occur before the transition is finished, but phase 3 does not visibly re-check table relkind, existence, primary key/replica identity, and replica identity FULL immediately before publication/catalog state is committed. | Concurrent DDL can alter replica identity or table shape after eligibility but before WAL mode is recorded. WAL decoding then runs under assumptions that are no longer true. | Hold a per-source advisory lock across transition phases or re-check eligibility immediately before `Transitioning`/WAL catalog updates. Add E2E tests that change replica identity/drop a PK during transition and assert trigger fallback or transition failure. |
| COR-04 | Medium | `src/dvm/operators/aggregate.rs` (`is_algebraically_invertible`), `tests/e2e_coverage_parser_tests.rs` | `SUM(CASE ...)` is forced to GROUP_RESCAN only when the aggregate argument is `Expr::Raw` whose trimmed string starts with `CASE`. The existing E2E case expression test creates the stream table in FULL mode and checks initial results, not differential update/delete behavior. | CASE expressions can be represented differently or wrapped in casts/functions. If detection misses one, the algebraic old-value formula can miscount UPDATEs where the CASE predicate depends on changed columns. | Detect CASE at the parsed AST level or normalize expression trees before classification. Add differential E2E tests for `SUM(CASE ...)` with INSERT, DELETE, and UPDATE crossing the CASE predicate threshold. |
| COR-05 | Medium | `src/dvm/operators/aggregate.rs` (`child_has_full_join`, aggregate fallback paths), `tests/e2e_full_join_tests.rs` | FULL JOIN aggregate handling has targeted fallback logic, but the decision is coarse and the high-risk combinations are nested FULL JOIN + aggregate + UPDATE/delete cycles. Tests cover FULL JOINs and some FULL JOIN aggregation, but not enough branch combinations for nested/rescan interactions. | FULL JOIN null-padding transitions are a common source of over-retraction and phantom rows. A missed fallback case can create drift that only appears after multi-cycle changes. | Add property-style E2E tests comparing DIFF vs FULL for nested FULL JOIN aggregates across multi-cycle insert/update/delete sequences, including NULL keys and both-side changes in the same cycle. |
| COR-06 | Medium | `src/scheduler/pool.rs` (`pg_trickle_pool_worker_main`), `src/scheduler/mod.rs` scheduler loop | The scheduler loop checks `pg_trickle.enabled`, but persistent pool workers do not check it inside their polling loop. They can remain alive and claim queued jobs if jobs already exist. | Operators may expect `pg_trickle.enabled = off` to stop all refresh execution. Existing queued work can still run or pool workers can keep consuming resources. | Check `config::pg_trickle_enabled()` before claiming each job in pool workers, and cancel or defer queued jobs when disabled. Add an E2E test with `worker_pool_size > 0`, queued work, and `pg_trickle.enabled = off`. |
| COR-07 | Medium | `src/wal_decoder.rs` (`poll_wal_changes`, `write_decoded_change`) | WAL polling and decoded-change writes rely on dynamic SQL construction, including slot names in `pg_logical_slot_get_changes` and manual escaping for values in INSERT SQL. Some inputs are internally generated, but the pattern is brittle. | Correctness failures appear as decoder errors, broken value round-tripping, or edge-case corruption when values contain unexpected encodings, backslashes, bytea-like text, or plugin output surprises. | Parameterize every value that can be a SPI parameter. For identifiers, centralize strict identifier constructors and assert slot-name grammar. Add WAL decoder tests with quotes, backslashes, unicode, large text, nulls, and bytea-like payloads. |
| COR-08 | Low | `src/dvm/operators/scan.rs` comments around EC-06 | Comments still state that a full EC-06 keyless duplicate fix requires future changes, but current code implements keyless net-counting and explicit keyless DML paths with E2E coverage. | Future maintainers may rework already-fixed behavior or mis-prioritize stale TODOs. | Replace stale comments with a current design note explaining keyless net-counting, non-unique row-id indexes, and remaining known limitations if any. |
| COR-09 | Low | `src/scheduler/mod.rs` (`execute_worker_atomic_group`, singleton execution) | Atomic groups use repeatable-read mode only for specific grouped jobs. Singleton refresh behavior appears intentional, but the isolation contract is not explicit in code-level docs. | Future changes to job grouping or dependency scheduling can unknowingly weaken snapshot consistency between related ST refreshes. | Document the isolation invariants for singleton, atomic group, repeatable-read group, cyclic SCC, immediate closure, and fused-chain jobs. Add targeted tests for snapshot consistency across grouped and ungrouped refreshes. |

Positive correctness findings:

| Area | Evidence |
| --- | --- |
| Keyless duplicates | `src/api/mod.rs` creates non-unique row-id indexes for keyless/partitioned storage; `src/dvm/operators/scan.rs` has keyless net-counting; `src/refresh/merge/mod.rs` uses explicit DML for keyless apply; `tests/e2e_keyless_duplicate_tests.rs` covers duplicate insert/delete/update/stress cases. |
| Manual refresh locking | `src/api/mod.rs` reloads `StreamTableMeta::get_by_name` after advisory and row locks in `refresh_stream_table_impl`. |
| Full refresh durability | `src/refresh/merge/mod.rs` wraps full refresh in transactional operations and captures diffs for downstream buffers. |
| Panic discipline | Targeted scans did not find obvious non-test direct `unwrap()` / `panic!()` hot spots in the main `src/` production paths searched. |

## Dimension 2 - Code Quality and Maintainability

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| CQ-01 | High | `src/api/mod.rs`, `src/scheduler/mod.rs`, `src/dvm/parser/sublinks.rs`, `src/cdc.rs` | Several files are multi-thousand-line modules mixing SQL APIs, validation, DDL, scheduling, worker orchestration, parsing, deparsing, and tests. | High cognitive load increases review misses in correctness-critical paths. Refactors become risky because local invariants are hard to see. | Continue the existing split into submodules: separate API lifecycle, alter/create validation, storage DDL, worker entry points, job execution, parser extraction, deparsing, and SubLink rewriting. Move large test modules into focused files. |
| CQ-02 | High | `src/cdc.rs`, `src/wal_decoder.rs`, `src/dvm/parser/rewrites.rs`, `src/refresh/codegen.rs`, `src/api/helpers.rs` | Dynamic SQL construction is decentralized. Some paths use `quote_ident`, some use `format('%I')`, some use manual `'` escaping, and some use SPI parameters. | Security and correctness depend on each call site remembering the right quoting mode. This is easy to regress. | Create a small SQL-building module with explicit helpers for identifiers, qualified names, literals, regclass casts, and SPI parameter wrappers. Ban ad hoc manual escaping in review/CI with a focused lint script. |
| CQ-03 | High | `scripts/gen_catalogs.py`, `docs/SQL_API_CATALOG.md`, `docs/GUC_CATALOG.md` | The catalog generator is regex-based and misses real APIs/GUC registrations. `_FN_SIG_RE` expects `pub fn`, but many `#[pg_extern]` functions are private. GUC statics are far from `GucRegistry::define_*` calls. | Generated docs are trusted but wrong. Missing functions and `(registration pending ...)` rows hide real public surface. | Replace regex extraction with a Rust-side manifest produced at build time, or parse Rust using `syn`. Add a CI check that regenerates catalogs and fails on diffs or pending registrations. |
| CQ-04 | Medium | `src/config.rs`, docs, `src/dvm/operators/scan.rs` | Naming/comments drift: config comments mention `pgtrickle.` while actual GUCs are `pg_trickle.*`; keyless comments describe an unfixed state; docs use old worker names. | Maintainers and users waste time chasing outdated concepts. | Run a stale-term audit each release. Add a simple docs linter for retired names: `pg_trickle.max_workers`, `pg_trickle.max_parallel_refresh_workers`, `event_driven_wake` as active, and `repair_stream_table` until implemented. |
| CQ-05 | Medium | `src/dvm/parser/*` | The parser/deparser layer uses many safe wrappers and SAFETY comments, but the unsafe parse-tree surface is very large and spread across many functions. | A subtle PostgreSQL parse-tree layout change or null-pointer assumption can become UB or wrong SQL. | Keep unsafe access behind a smaller typed facade. Add focused fuzzing for deparser round-trips and unsupported node handling, then run those fuzz targets in CI. |
| CQ-06 | Medium | `src/scheduler/mod.rs` launcher and worker code | Background worker startup paths often convert SPI errors to empty/default values with `unwrap_or` style fallbacks. | Operational faults can look like normal absence: a DB may be skipped or a worker may silently continue with default state. | Preserve fail-open behavior where needed, but log structured warnings with the database name, query, and error class. Consider counters exposed through diagnostics. |
| CQ-07 | Medium | `src/api/mod.rs` (`create_stream_table_impl` and related validation) | `create_stream_table` now carries 16 parameters plus bulk/create-or-replace variants. Validation and defaulting are duplicated across function variants and JSON paths. | Parameter drift has already hit docs and can hit behavior: a new option may be handled in one API path but not another. | Introduce a `CreateStreamTableOptions` struct used by SQL, JSON bulk, and create-or-replace paths. Centralize defaults, validation, docs generation, and serialization. |
| CQ-08 | Low | `src/shmem.rs`, `src/dag.rs`, `src/scheduler/*` | Many comments encode milestone IDs (`SCAL-5`, `C2-1`, `DI-8`) without always explaining current invariants independent of roadmap history. | Historical breadcrumbs are useful, but they can obscure the present contract. | Keep milestone IDs only when they link to a design doc; otherwise replace with current invariant comments. |

## Dimension 3 - Performance

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| PERF-01 | High | `src/dvm/operators/join.rs` (`PART3_MAX_SCAN_COUNT`, `DEEP_JOIN_L0_SCAN_THRESHOLD`), `src/refresh/codegen.rs` thresholds | Deep join behavior is governed by hardcoded thresholds. Comments mention severe temp-file behavior for deep joins, but users cannot tune thresholds per workload. | Workloads can flip from fast differential execution to massive rescans or temp-file spills with no clear operator control. | Promote deep-join thresholds and Part 3 branch limits to documented GUCs with sane defaults. Emit diagnostics when a query crosses thresholds and include EXPLAIN snippets in validation output. |
| PERF-02 | High | `src/dvm/operators/aggregate.rs` GROUP_RESCAN paths | Fallbacks for `SUM(CASE)`, MIN/MAX-like aggregates, and FULL JOIN aggregate combinations can require rescanning affected groups via EXCEPT ALL/rescan logic. | Large groups with small deltas lose the core O(delta) advantage and can become refresh latency cliffs. | Implement DI-2-style old/new UPDATE split for mutable aggregate arguments. Track per-aggregate fallback mode in diagnostics and benchmarks. |
| PERF-03 | Medium | `src/wal_decoder.rs` (`MAX_CHANGES_PER_POLL = 10_000`, `MAX_LAG_BYTES = 65_536`) | WAL poll batch size and transition lag tolerance are constants rather than workload-aware or GUC-tunable. | High-throughput systems can either lag unnecessarily or churn through too-small batches; low-latency systems cannot tune conservatively. | Expose batch size, max lag, and error thresholds as GUCs with bounds. Add metrics for poll duration, rows decoded, lag bytes, and fallback reason. |
| PERF-04 | Medium | `src/shmem.rs` (`COST_CACHE_CAPACITY = 256`) | The shared cost model cache has 256 direct-mapped slots and falls back to SPI on collisions/misses. Comments estimate low collision for <= 1,000 STs, but docs describe larger deployments. | Large installations can lose the intended shmem fast path and add catalog/SPI load during scheduling. | Make capacity configurable at preload time or switch to a small associative cache. Expose hit/miss/collision metrics per tick. |
| PERF-05 | Medium | `src/scheduler/mod.rs`, `src/scheduler/pool.rs`, `src/config.rs` | Spawn-per-task dynamic workers remain the default (`worker_pool_size = 0`) even though persistent pool support exists. | High-frequency refresh workloads pay repeated background worker registration/startup overhead unless users know to enable the pool. | Benchmark spawn-per-task vs persistent pool in CI and docs. Consider auto-enabling the pool above a refresh-rate or DAG-size threshold. |
| PERF-06 | Medium | `src/refresh/merge/mod.rs`, `src/cdc.rs` | Change buffers and explicit DML paths are correctness-oriented, but high write amplification remains for UPDATEs, keyless tables, and trigger CDC. | Source tables with heavy UPDATE/DELETE volume can incur high WAL, index, and vacuum pressure. | Add benchmark dimensions for UPDATE-heavy and keyless workloads. Document expected buffer growth and autovacuum settings. Consider compaction thresholds by source table churn. |
| PERF-07 | Low | `benches/*`, `.github/workflows/ci.yml`, `.github/workflows/e2e-benchmarks.yml` | Criterion and E2E benchmarks exist, and PR benchmark regression checks exist for main-targeted PRs, but broader E2E benchmark workflows are scheduled/manual and many jobs are non-blocking. | Performance regressions in expensive paths may be noticed after merge rather than before. | Keep the quick PR gate, but add targeted mandatory microbenchmarks for join codegen, placeholder resolution, scan/aggregate delta generation, and scheduler DAG rebuild. |

## Dimension 4 - Scalability

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| SCAL-01 | High | `src/shmem.rs` (`INVALIDATION_RING_CAPACITY = 128`) | The invalidation ring is fixed at 128 pgt_ids. Overflow triggers a full DAG rebuild on the next scheduler drain. | Large migrations/dbt rebuilds can repeatedly fall back to O(V+E) rebuilds and create refresh stalls. Correctness is protected, but latency is not. | Make the ring capacity preload-configurable or use a growable catalog-backed invalidation queue. Track overflow count and expose it in monitoring. |
| SCAL-02 | High | `src/scheduler/mod.rs` launcher model, docs | The launcher spawns one scheduler per database with pg_trickle installed, plus optional refresh workers. This competes for `max_worker_processes` with autovacuum, parallel query, logical replication, and other extensions. | Multi-database clusters can silently stop refreshing databases when worker slots are exhausted; docs warn about this, but runtime admission and visibility are limited. | Add a preflight health function that computes required worker slots from installed databases and GUCs. Surface worker-slot exhaustion as SQL-visible status, not only logs. |
| SCAL-03 | Medium | `src/dag.rs` (`rebuild_incremental`) | Incremental DAG rebuild removes and reloads affected nodes, but still re-resolves CALCULATED schedules with O(V) worst-case iterations. | Very large DAGs can still pay full-graph costs after small changes, especially under frequent DDL. | Maintain dependency-local schedule invalidation and cache calculated schedule results by generation. Add large DAG benchmarks for incremental rebuild under repeated ALTER/CREATE/DROP. |
| SCAL-04 | Medium | `src/shmem.rs` cost cache, `src/scheduler/cost.rs` quota logic | Worker quota and cost-model structures are cluster-wide but relatively simple. There is no durable global broker for fair scheduling across many databases. | Busy databases can dominate refresh capacity when quotas are disabled; quotas are static and not tied to observed lag/staleness. | Add lag-aware scheduling across databases, or graduate the research broker design into a real shared registry. Persist fairness state across scheduler restarts. |
| SCAL-05 | Medium | `src/scheduler/citus.rs` | Citus worker failure tracking is thread-local in the scheduler worker (`CITUS_WORKER_FAILURES`). | Scheduler restart loses consecutive failure history and may delay alerting or recovery decisions for distributed sources. | Persist failure counters in catalog or shared memory with timestamps, and expose them through diagnostics. |
| SCAL-06 | Medium | `src/wal_decoder.rs`, docs | WAL CDC uses replication slots per eligible source. Docs mention `max_replication_slots`, but runtime capacity planning appears reactive to slot-creation errors rather than proactive. | Large source counts can hit slot limits during rollout, causing partial WAL adoption and mixed CDC modes. | Add a preflight check before enabling WAL/auto mode across many sources. Report required slots, available slots, and sources blocked by capacity. |
| SCAL-07 | Low | `docs/CAPACITY_PLANNING.md`, `docs/SCALING.md`, `cnpg/cluster-example.yaml` | Docs describe medium/large deployments, but examples still use low `max_worker_processes` and commented resource requests. | Users may copy examples that underprovision production clusters. | Ship separate dev/small/large manifests with explicit worker, memory, CPU, WAL, and autovacuum budgets. |

## Dimension 5 - Feature Gaps

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| FEAT-01 | High | `docs/SQL_REFERENCE.md`, `blog/backup-and-restore.md`, `docs/GETTING_STARTED.md`, `docs/ERRORS.md`; no implementation in `src/api/**` | `pgtrickle.repair_stream_table(name)` is documented as the post-restore repair path, but no implementation was found. | Backup/restore instructions are not actionable. After restore, users can be left with broken frontiers/change buffers and no documented function to repair them. | Implement `repair_stream_table` or remove/replace the docs immediately. The function should reinitialize storage, reset frontiers, rebuild CDC triggers/buffers, and verify dependencies. Add restore E2E coverage. |
| FEAT-02 | High | `docs/SQL_REFERENCE.md`, `src/api/mod.rs` | Public docs do not match the current `create_stream_table` signature. The implementation accepts 16 parameters, while the SQL reference signature shows fewer and omits newer knobs. | Users cannot discover or correctly use `output_distribution_column`, `temporal`, `storage_backend`, and other advanced options from the primary SQL reference. | Generate SQL reference signatures from the pgrx manifest or a single source-of-truth options struct. Add docs tests that compare documented signatures to actual `#[pg_extern]` functions. |
| FEAT-03 | Medium | `src/api/mod.rs`, `src/api/helpers.rs`, docs | Some newer storage backends/features (`storage_backend`, temporal mode, Citus distribution) are wired into API/catalog paths, but documentation is spread across SQL reference, upgrading notes, Citus docs, and configuration docs. | Features exist but are hard to operate safely, especially with extension prerequisites and failure modes. | Add a single `Storage Backends` reference page with prerequisites, validation, unsupported combinations, migration behavior, and fallback semantics. |
| FEAT-04 | Medium | `src/api/diagnostics.rs`, docs | Diagnostics exist, but there is no single operator-facing explanation for why a stream table used FULL fallback, GROUP_RESCAN, deep-join snapshots, or keyless explicit DML in a given refresh. | Users cannot tell whether a slow refresh is expected, a regression, or a query-design issue. | Add `pgtrickle.explain_stream_table(name)` or extend existing diagnostics to report chosen DVM plan, fallback reasons, thresholds crossed, and expected complexity. |
| FEAT-05 | Medium | `src/wal_decoder.rs`, `docs/CDC_MODES.md` | WAL CDC supports transition/fallback, but operator controls are coarse. Batch size, lag thresholds, retry policy, and per-source blocked reasons should be first-class. | WAL adoption at scale is harder to tune and troubleshoot. | Add per-source WAL status with `blocked_reason`, slot lag, publication state, last decoder error, and tunable GUCs. |
| FEAT-06 | Low | `dbt-pgtrickle`, docs | The project supports dbt integration, but docs and API drift mean dbt macros can lag new create options. | Analytics users may miss new features or produce inconsistent ST definitions. | Generate dbt macro option schemas from the same `CreateStreamTableOptions` source used by Rust and docs. |

## Dimension 6 - Test Coverage and Quality

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| TEST-01 | High | `tests/**/*.rs` | A search found 116 fixed sleeps in tests, including WAL CDC, scheduler, cascade, PgBouncer, quota, and bgworker tests. | Fixed sleeps make tests slow and flaky: they can fail under load and waste time when the expected state is already reached. | Replace sleeps with polling helpers that wait for explicit states: refresh history row, CDC mode, scheduler tick watermark, job status, NOTIFY, or catalog status. Keep short sleeps only for deliberate debounce tests. |
| TEST-02 | High | `tests/e2e_coverage_parser_tests.rs`, `src/dvm/operators/aggregate.rs` | `SUM(CASE ...)` has only a FULL-mode initial-result coverage example, not a differential update/delete regression test for the DI-8 fallback. | The highest-risk part of the SUM(CASE) fix can regress without test failure. | Add DIFF-mode tests where UPDATE crosses the CASE condition, DELETE removes qualifying/non-qualifying rows, and INSERT adds both classes. Compare stream table with full query after each cycle. |
| TEST-03 | High | `tests/e2e_wal_cdc_tests.rs`, `src/wal_decoder.rs` | WAL tests cover lifecycle and keyless replica identity checks, but not concurrent DDL during transition. | The TOCTOU transition bug class remains untested. | Add tests that start WAL transition, concurrently change replica identity/drop PK/drop source, then assert safe fallback, error status, or trigger mode with no data loss. |
| TEST-04 | Medium | `src/dvm/mod.rs`, `src/refresh/codegen.rs` | Placeholder resolution lacks visible direct unit tests for unresolved tokens and mixed source frontiers. | Template-cache changes can introduce latent SQL failures. | Add pure unit tests for both resolver functions, including unknown placeholder families, missing OIDs, repeated placeholders, pgt-prefixed placeholders, and zero-change pruning. |
| TEST-05 | Medium | `scripts/gen_catalogs.py`, docs | Generated docs are not protected by regression tests. The current SQL API and GUC catalogs are visibly stale/broken. | Docs can drift indefinitely without CI failure. | Add CI job: run generator, fail if output changes, fail on `(registration pending`, and assert known functions like `create_stream_table` appear. |
| TEST-06 | Medium | `fuzz/*`, `.github/workflows/*` | Fuzz targets exist for parser, cron, GUC, CDC, WAL, and DAG, but no workflow runs `cargo fuzz` or corpus regression in CI. | Parser/WAL/CDC fuzzing value depends on manual execution. | Add a scheduled fuzz smoke job with short time budgets per target and artifact upload for crashes/corpus. Run parser and WAL targets on PRs with a very small budget if feasible. |
| TEST-07 | Medium | `.github/workflows/ci.yml` | Windows only compile-checks unit tests with `cargo test --lib --features pg18 --no-run`, is scheduled/manual only, and is `continue-on-error: true`. macOS is also scheduled/manual only. | Platform regressions can land. Windows runtime behavior is never validated by the main PR gate. | Make Windows compile failures blocking on scheduled runs, then graduate key pure unit tests to PR. Keep pgrx/Postgres-specific integration on Linux if Windows DB setup is impractical. |
| TEST-08 | Medium | `.github/workflows/coverage.yml` | E2E coverage job comments say scheduled weekly, but the job condition only allows `workflow_dispatch`. Unit coverage uploads are non-blocking. | Coverage blind spots in DVM/CDC paths are not continuously measured. | Re-enable weekly E2E coverage or correct comments if intentionally manual. Track coverage by module and require non-decreasing coverage for core DVM/CDC files. |
| TEST-09 | Low | `tests/e2e_keyless_duplicate_tests.rs` | Keyless duplicate tests are strong, but stale EC-06 comments suggest maintainers may not know they are the canonical coverage. | Future changes may bypass or duplicate the wrong path. | Reference these tests from code comments and architecture docs. Add a small property test for keyless multiset equivalence. |

Positive test findings:

| Area | Evidence |
| --- | --- |
| Keyless duplicate coverage | Dedicated E2E file covers duplicate inserts, delete one duplicate, update one duplicate, aggregates, unique baselines, delete/reinsert, and stress. |
| FULL JOIN coverage | Multiple DVM/E2E files cover basic, nested, natural-style, NULL-key, and multi-cycle FULL JOIN behavior. |
| Upgrade coverage | Upgrade scripts and E2E upgrade tests exist across historical versions. |
| Bench/fuzz assets | Criterion benches and libFuzzer target definitions exist; the gap is mostly automation. |

## Dimension 7 - Security

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| SEC-01 | High | `src/ivm.rs` generated IVM trigger functions | IVM trigger functions are SECURITY DEFINER and set `search_path = pgtrickle_changes, pgtrickle, pg_catalog, pg_temp, public`. The inclusion of `public` weakens the otherwise good SECURITY DEFINER posture. | If generated delta SQL resolves unqualified names through `public`, a lower-privileged user with create rights can influence function/operator/table resolution in a definer context. | Remove `public` from SECURITY DEFINER search_path where possible. Schema-qualify user tables/functions or capture the creator search path safely at create time. If `public` is required, document why and restrict CREATE on public in install guidance. |
| SEC-02 | Medium | `src/wal_decoder.rs`, `src/cdc.rs`, `src/dvm/parser/rewrites.rs` | Dynamic SQL uses a mixture of manual escaping, formatted identifiers, and parameters. Some values are internally constrained, but the pattern is inconsistent. | SQL injection risk is likely low for OID-derived names but higher for future changes. Manual escaping is also error-prone for correctness. | Centralize identifier/literal handling and parameterize everything that is not an identifier. Add semgrep or custom CI checks for `format!("SELECT ... '{}'")` patterns in production Rust. |
| SEC-03 | Medium | `docs/SQL_REFERENCE.md` RLS section, `src/api/mod.rs`, `src/ivm.rs` | Source-table RLS bypass during refresh/IMMEDIATE mode is documented, but it is a major security semantic: source RLS does not protect derived stream-table contents. | Users can accidentally expose rows through stream tables if they rely on source RLS and forget to apply RLS on stream tables. | Add creation-time WARNING/NOTICE when source tables have RLS enabled. Provide helper SQL to mirror RLS policies or validate that stream-table RLS exists before enabling refresh. |
| SEC-04 | Medium | `monitoring/docker-compose.yml`, `monitoring/README.md` | Monitoring demo uses `POSTGRES_PASSWORD=postgres` and Grafana `admin/admin`, and documents those defaults. | Safe for local demos, unsafe if copied to shared environments. | Use environment-variable defaults with a `.env.example`, add comments that credentials must be changed, and bind services to localhost in examples where possible. |
| SEC-05 | Low | `pg_trickle.control` | Extension is `superuser = true` and `trusted = false`, which is appropriate for current capabilities but limits least-privilege adoption. | Managed/hosted Postgres environments may not allow installation; users may request unsafe workarounds. | Keep superuser requirement unless the SQL surface is audited for trusted installation. Document exact privileges needed and why trusted install is not supported. |
| SEC-06 | Low | `src/lib.rs`, `src/cdc.rs`, `sql/*.sql` | Most SECURITY DEFINER functions correctly set explicit search paths. This is a positive finding, not a gap, but it should be kept under test. | Future definer functions can regress if no CI check exists. | Add a SQL/static check that every SECURITY DEFINER body includes `SET search_path` and does not include user-writable schemas unless justified. |

## Dimension 8 - Operational and Deployment Readiness

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| OPS-01 | High | `Dockerfile.hub`, `Dockerfile.ghcr`, `Cargo.toml` | Public-image Dockerfiles default to stale `ARG VERSION` values (`0.11.0`, `0.13.0`) while the package version is `0.40.0`. | Images built without explicit build args get misleading OCI labels and possibly release metadata. | Derive VERSION from `Cargo.toml` during build or require the release workflow to pass it and fail if it differs from Cargo. |
| OPS-02 | Medium | `Dockerfile.demo`, `Dockerfile.hub`, `Dockerfile.ghcr`, `tests/Dockerfile.*` | Runtime Dockerfiles do not define container `HEALTHCHECK`s. | Orchestrators cannot detect Postgres/extension readiness from the image alone. | Add healthchecks based on `pg_isready` and, for demo images, optional `SELECT pgtrickle.health_check()` once the extension is installed. |
| OPS-03 | Medium | `cnpg/cluster-example.yaml` | CNPG example sets `max_worker_processes: "8"` and comments out resource requests/limits. The comments warn about pg_trickle workers but the default example remains underprovisioned for many real deployments. | Users can copy an example that stalls schedulers or competes with autovacuum/parallel query. | Provide dev and production CNPG examples. Production example should include worker budget formula, resource requests/limits, WAL sizing, storage class guidance, and monitoring hooks. |
| OPS-04 | Medium | `docs/PRE_DEPLOYMENT.md`, `docs/FAQ.md`, `src/scheduler/mod.rs` | Worker exhaustion is mostly visible through logs and docs, not as a hard preflight failure. | Databases can stop refreshing with little SQL-visible evidence. | Add `pgtrickle.preflight()` and `pgtrickle.worker_pool_status()` checks that flag insufficient `max_worker_processes`, replication slots, WAL level, missing preload, and disabled scheduler. |
| OPS-05 | Medium | `docs/SQL_REFERENCE.md`, backup/restore docs, missing `repair_stream_table` | Restore guidance depends on a missing repair function. | Restore runbooks fail at the most stressful moment. | Either implement repair before release or replace all docs with the actual supported restore procedure. Add a restore drill E2E test. |
| OPS-06 | Low | `monitoring/*` | Monitoring stack is useful but demo-oriented; credentials and service exposure are not production-grade. | Users may overtrust demo manifests. | Split `monitoring/demo` and `monitoring/production` guidance. Add TLS/auth notes and least-privilege exporter role setup. |

## Dimension 9 - Documentation

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| DOC-01 | High | `docs/SQL_API_CATALOG.md`, `scripts/gen_catalogs.py` | SQL API catalog claims only 24 SQL-callable functions and misses private `#[pg_extern] fn` items such as current lifecycle APIs. | Public API inventory is wrong. Reviewers and users cannot trust the catalog. | Fix extraction, regenerate, and gate in CI. Include schema, volatility, security, args, defaults, and deprecation status. |
| DOC-02 | High | `docs/GUC_CATALOG.md`, `scripts/gen_catalogs.py`, `src/config.rs` | GUC catalog shows many `(registration pending - PGS_...)` rows because generator cannot link statics to later registrations. | Configuration docs look unfinished and hide actual GUC names. | Generate from `GucRegistry::define_*` calls or a declarative GUC table in Rust. Fail CI on any pending registration. |
| DOC-03 | High | `docs/SQL_REFERENCE.md`, `src/api/mod.rs` | `create_stream_table` signature and parameter table are stale compared with the 16-argument implementation. | Users cannot discover supported parameters and examples can be wrong. | Generate signatures from source or options schema. Update examples for `output_distribution_column`, `temporal`, `storage_backend`, and current bulk JSON keys. |
| DOC-04 | High | `docs/SQL_REFERENCE.md`, `blog/backup-and-restore.md`, `docs/GETTING_STARTED.md`, `docs/ERRORS.md` | `repair_stream_table` is documented but missing. | Docs promise operational recovery that the product cannot perform. | Remove or implement, then add linkable runbook tests. |
| DOC-05 | Medium | `docs/CONFIGURATION.md`, `src/config.rs` | Deprecated/no-effect GUCs such as `event_driven_wake` and `wake_debounce_ms` still appear in tuning guidance as active knobs. | Operators can spend time tuning settings that do nothing. | Move deprecated GUCs to a dedicated compatibility appendix and remove them from tuning tables. |
| DOC-06 | Medium | `docs/PRE_DEPLOYMENT.md`, `docs/PATTERNS.md`, `docs/SCALING.md`, `docs/integrations/multi-tenant.md` | Docs reference stale/nonexistent worker names such as `pg_trickle.max_workers` and `pg_trickle.max_parallel_refresh_workers`; code uses `max_dynamic_refresh_workers`, `max_concurrent_refreshes`, `worker_pool_size`, and quota GUCs. | Capacity planning guidance becomes internally inconsistent. | Run a docs-wide rename audit. Add CI grep checks for retired GUC names. |
| DOC-07 | Medium | `docs/ARCHITECTURE.md` | Architecture docs still refer to old single-file modules like `src/refresh.rs` and `src/scheduler.rs`, while the repo uses submodule directories. | New contributors start from an inaccurate map. | Update module diagrams to the current layout: `src/api/mod.rs`, `src/refresh/merge/mod.rs`, `src/scheduler/mod.rs`, parser submodules, etc. |
| DOC-08 | Low | `docs/SQL_REFERENCE.md` RLS section | RLS bypass is documented, which is good, but should be more prominent in setup and security docs. | Security-sensitive users may miss it in the long SQL reference. | Duplicate the warning in Getting Started, Pre-Deployment, and Security docs. |

## Dimension 10 - CI and Developer Experience

| ID | Severity | Location | Description | Impact | Recommended fix |
| --- | --- | --- | --- | --- | --- |
| CI-01 | High | `.github/workflows/ci.yml` | macOS and Windows jobs are scheduled/manual only; Windows is compile-only and `continue-on-error: true`. | Cross-platform regressions are not PR-blocking, and Windows runtime behavior is not tested. | Make scheduled Windows failures blocking first, then add PR compile checks for pure Rust modules. Keep Linux as DB-heavy gate if needed. |
| CI-02 | High | `.github/workflows/ci.yml` | Full E2E tests are schedule/manual only and explicitly skipped on PRs and push-to-main. Light E2E is a strong PR gate, but custom-image/full paths are not continuously merge-gated. | Docker image, full extension packaging, and heavier E2E regressions can merge before detection. | Add a smaller required full-image smoke test on PR/push: build image, install extension, run a representative DVM/CDC/scheduler scenario. Keep exhaustive full E2E scheduled/manual. |
| CI-03 | Medium | `.github/workflows/coverage.yml` | E2E coverage comments say weekly, but job runs only on manual dispatch. Codecov upload is non-blocking. | Coverage rot is invisible in high-risk integration paths. | Restore scheduled E2E coverage or fix docs. Add module-level coverage reporting for DVM, CDC, WAL, scheduler. |
| CI-04 | Medium | `.github/workflows/benchmarks.yml`, `.github/workflows/ci.yml` | Bencher workflow is disabled except manual. CI has a quick Criterion PR gate for main-targeted PRs, while broader benchmarks are scheduled/manual and non-blocking. | Some performance regressions are caught, but only within the quick benchmark envelope. | Keep the quick gate and add focused mandatory benches for codegen, join delta construction, aggregate fallback, and DAG rebuild. |
| CI-05 | Medium | `fuzz/Cargo.toml`, `.github/workflows/*` | Fuzz targets are defined but not run in GitHub Actions. | Parser/WAL/CDC fuzz regressions rely on manual effort. | Add scheduled fuzz smoke and corpus replay. Upload crashes and minimized repros as artifacts. |
| CI-06 | High | `scripts/gen_catalogs.py`, `docs/*CATALOG.md`, `.github/workflows/*` | Generated SQL/GUC catalogs are not checked for freshness or obvious errors. | Stale docs have already landed. | Add `just docs-check` or equivalent CI job that regenerates docs, fails on diff, and fails on pending/unknown markers. |
| CI-07 | Low | `justfile`, `AGENTS.md` | Developer guidance correctly requires `just fmt` and `just lint` after code changes, but doc-only changes do not have an equivalent docs validation command. | Docs regressions are easy to miss. | Add `just docs-lint`/`just docs-generate-check` and include it in contributor guidance. |

## Cross-Cutting Synthesis

### Systemic Themes

| Theme | Evidence | Recommended program-level response |
| --- | --- | --- |
| Correctness-sensitive complexity is concentrated in DVM template generation | Snapshot cache keys, LSN placeholders, aggregate fallback classification, FULL JOIN null-padding, keyless multiset semantics. | Treat DVM SQL generation as a compiler: structural hashes, invariants, exhaustive placeholder validation, golden tests, fuzzing, and explainable plans. |
| Documentation and implementation have diverged | Broken SQL API/GUC catalogs, stale SQL reference signatures, missing repair function, deprecated/no-effect GUCs in tuning docs, old module paths. | Make docs generated from source of truth wherever possible. Put generated-doc freshness in CI. |
| Operational safety depends too much on logs and tribal knowledge | Worker exhaustion, WAL slot capacity, RLS bypass, missing repair, demo credentials, low CNPG defaults. | Add preflight SQL functions, SQL-visible blocked reasons, and production-grade manifests. |
| Expensive/high-value tests are mostly outside the PR gate | Full E2E, E2E coverage, fuzzing, full benchmarks, Windows runtime tests. | Add tiny mandatory smoke slices for each expensive domain and keep exhaustive runs scheduled/manual. |
| There is strong recent hardening, but stale comments obscure it | Keyless duplicate support is implemented but comments suggest otherwise; manual refresh race appears fixed. | Delete or update stale TODOs aggressively. Maintain an ADR/status page for fixed historical hazards. |

### Release Readiness Assessment

| Category | Assessment |
| --- | --- |
| Correctness | Good for many common workloads; still needs hardening around DVM structural cache keys, unresolved placeholders, WAL transition concurrency, and aggregate fallback coverage before claiming world-class correctness. |
| Performance | Strong ambitions and useful benchmarks, but deep joins and GROUP_RESCAN fallback paths remain likely cliffs. More tunability and mandatory microbench gates are needed. |
| Scalability | Solid DAG and worker-pool foundations, but fixed shared-memory capacities and worker-slot pressure need better preflight and visibility. |
| Security | Mostly careful SECURITY DEFINER posture. IVM `public` search_path and dynamic SQL patterns need hardening. RLS bypass must be made impossible to miss. |
| Operations | Good monitoring/demo materials, upgrade scripts, and CNPG examples exist, but restore repair is missing and examples need production-safe variants. |
| Documentation | Not release-grade until SQL/GUC catalogs and SQL reference are regenerated and stale names/functions are fixed. |
| CI | Strong Linux/light-E2E base, but not enough continuous coverage of full image, fuzzing, cross-platform, generated docs, and expensive performance paths. |

### Suggested Fix Order

| Order | Work item | Rationale |
| --- | --- | --- |
| 1 | Fix DVM snapshot cache key and placeholder validation | Highest direct risk of silent wrong results. |
| 2 | Add WAL transition final recheck and concurrency tests | Protects CDC mode transitions from DDL races. |
| 3 | Implement or remove `repair_stream_table` | Backup/restore docs currently promise a non-existent recovery function. |
| 4 | Fix docs generation and gate generated docs in CI | Prevents recurring public API drift. |
| 5 | Add differential SUM(CASE), placeholder, and WAL transition tests | Locks down recent correctness patches. |
| 6 | Replace fixed sleeps with state polling in highest-flake E2E files | Improves trust in CI and shortens runtime. |
| 7 | Promote deep-join/WAL thresholds to GUCs and diagnostics | Turns performance cliffs into tunable behavior. |
| 8 | Harden IVM SECURITY DEFINER search path and dynamic SQL helpers | Reduces future security regression surface. |
| 9 | Add production-grade CNPG/monitoring/Docker defaults | Makes examples safe to copy. |
| 10 | Add full-image PR smoke, fuzz smoke, and docs-generation CI | Closes the biggest release-gate holes without running the entire expensive suite on every PR. |

## Appendix - Files Analysed

Core Rust:

- `src/lib.rs`
- `src/api/mod.rs`
- `src/api/helpers.rs`
- `src/api/diagnostics.rs`
- `src/api/inbox.rs`
- `src/api/outbox.rs`
- `src/api/planner.rs`
- `src/api/snapshot.rs`
- `src/catalog.rs`
- `src/cdc.rs`
- `src/config.rs`
- `src/dag.rs`
- `src/diagnostics.rs`
- `src/error.rs`
- `src/hooks.rs`
- `src/ivm.rs`
- `src/monitor.rs`
- `src/scheduler/mod.rs`
- `src/scheduler/pool.rs`
- `src/scheduler/cost.rs`
- `src/scheduler/citus.rs`
- `src/shmem.rs`
- `src/template_cache.rs`
- `src/version.rs`
- `src/wal_decoder.rs`

DVM and refresh:

- `src/dvm/mod.rs`
- `src/dvm/diff.rs`
- `src/dvm/operators/aggregate.rs`
- `src/dvm/operators/join.rs`
- `src/dvm/operators/scan.rs`
- `src/dvm/parser/mod.rs`
- `src/dvm/parser/rewrites.rs`
- `src/dvm/parser/sublinks.rs`
- `src/dvm/parser/types.rs`
- `src/dvm/parser/validation.rs`
- `src/refresh/codegen.rs`
- `src/refresh/merge/mod.rs`
- `src/refresh/merge/columns.rs`
- `src/refresh/merge/update.rs`
- `src/refresh/phd1.rs`

Tests and benches:

- `tests/e2e_keyless_duplicate_tests.rs`
- `tests/e2e_wal_cdc_tests.rs`
- `tests/e2e_full_join_tests.rs`
- `tests/e2e_join_tests.rs`
- `tests/e2e_multi_cycle_tests.rs`
- `tests/e2e_diff_full_equivalence_tests.rs`
- `tests/e2e_coverage_parser_tests.rs`
- `tests/e2e_dag_operations_tests.rs`
- `tests/e2e_dag_autorefresh_tests.rs`
- `tests/e2e_cascade_regression_tests.rs`
- `tests/e2e_upgrade_tests.rs`
- `tests/e2e_tpch_tests.rs`
- `tests/e2e_tpch_dag_tests.rs`
- `tests/dvm_full_join_tests.rs`
- `tests/dvm_nested_full_join_tests.rs`
- `tests/dvm_natural_join_tests.rs`
- `tests/dvm_aggregate_execution_tests.rs`
- `tests/property_tests.rs`
- `tests/common/mod.rs`
- `benches/diff_operators.rs`
- `benches/refresh_bench.rs`
- `benches/scheduler_bench.rs`
- `fuzz/Cargo.toml`
- `fuzz/fuzz_targets/parser_fuzz.rs`
- `fuzz/fuzz_targets/cdc_fuzz.rs`
- `fuzz/fuzz_targets/wal_fuzz.rs`
- `fuzz/fuzz_targets/dag_fuzz.rs`
- `fuzz/fuzz_targets/guc_fuzz.rs`
- `fuzz/fuzz_targets/cron_fuzz.rs`

Docs and generation:

- `README.md`
- `INSTALL.md`
- `AGENTS.md`
- `Cargo.toml`
- `pg_trickle.control`
- `scripts/gen_catalogs.py`
- `docs/ARCHITECTURE.md`
- `docs/SQL_REFERENCE.md`
- `docs/CONFIGURATION.md`
- `docs/GUC_CATALOG.md`
- `docs/SQL_API_CATALOG.md`
- `docs/CAPACITY_PLANNING.md`
- `docs/SCALING.md`
- `docs/PRE_DEPLOYMENT.md`
- `docs/TROUBLESHOOTING.md`
- `docs/GETTING_STARTED.md`
- `docs/ERRORS.md`
- `docs/CDC_MODES.md`
- `docs/CITUS.md`
- `docs/integrations/citus.md`
- `docs/integrations/pgbouncer.md`
- `docs/integrations/multi-tenant.md`
- `docs/integrations/cloudnativepg.md`
- `docs/UPGRADING.md`
- `blog/backup-and-restore.md`
- `blog/snapshots-time-travel.md`

SQL, CI, deployment, and operations:

- `sql/pg_trickle--0.1.3--0.2.0.sql` through `sql/pg_trickle--0.39.0--0.40.0.sql`
- `.github/workflows/ci.yml`
- `.github/workflows/coverage.yml`
- `.github/workflows/benchmarks.yml`
- `.github/workflows/e2e-benchmarks.yml`
- `.github/workflows/sqlancer.yml`
- `.github/workflows/tpch-nightly.yml`
- `.github/workflows/unsafe-inventory.yml`
- `Dockerfile.demo`
- `Dockerfile.hub`
- `Dockerfile.ghcr`
- `Dockerfile.relay`
- `tests/Dockerfile.e2e`
- `tests/Dockerfile.e2e-coverage`
- `tests/Dockerfile.e2e-upgrade`
- `tests/Dockerfile.e2e-base-lite`
- `tests/Dockerfile.builder`
- `monitoring/docker-compose.yml`
- `monitoring/README.md`
- `monitoring/prometheus/prometheus.yml`
- `monitoring/prometheus/alerts.yml`
- `monitoring/prometheus/pg_trickle_queries.yml`
- `monitoring/grafana/provisioning/datasources/prometheus.yml`
- `monitoring/grafana/dashboards/pg_trickle_overview.json`
- `cnpg/cluster-example.yaml`
- `cnpg/database-example.yaml`
- `cnpg/Dockerfile.ext`
- `cnpg/Dockerfile.ext-build`
