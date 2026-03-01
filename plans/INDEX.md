# plans/ — Document Index

Quick-reference inventory of all planning documents. Updated manually — add
new entries when creating documents.

**Type key:** PLAN = implementation plan · GAP = gap analysis · REPORT = research/assessment · ADR = architecture decision · STATUS = progress tracking

---

## plans/ (root)

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN.md](PLAN.md) | PLAN | — | Master implementation plan (Phases 0–12) |
| [PLAN_FEATURE_CLEANUP.md](PLAN_FEATURE_CLEANUP.md) | PLAN | In progress | Remove low-value surface before public release |

## adrs/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_ADRS.md](adrs/PLAN_ADRS.md) | ADR | Proposed | Collection of architecture decision records |

## dbt/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_DBT_ADAPTER.md](dbt/PLAN_DBT_ADAPTER.md) | PLAN | Proposed | dbt integration via full custom adapter |
| [PLAN_DBT_MACRO.md](dbt/PLAN_DBT_MACRO.md) | PLAN | Implemented | dbt integration via custom materialization macro |

## ecosystem/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [GAP_ANALYSIS_EPSIO.md](ecosystem/GAP_ANALYSIS_EPSIO.md) | GAP | — | Core SQL IVM engine comparison vs Epsio |
| [GAP_ANALYSIS_FELDERA.md](ecosystem/GAP_ANALYSIS_FELDERA.md) | GAP | — | Core SQL IVM engine comparison vs Feldera |
| [PLAN_CLOUDNATIVEPG.md](ecosystem/PLAN_CLOUDNATIVEPG.md) | PLAN | Implemented | CloudNativePG image volume extension |
| [PLAN_ECO_SYSTEM.md](ecosystem/PLAN_ECO_SYSTEM.md) | PLAN | Proposed | Supportive projects ecosystem plan |
| [GAP_PG_IVM_COMPARISON.md](ecosystem/GAP_PG_IVM_COMPARISON.md) | GAP | Reference | pg_trickle vs pg_ivm comparison & gap analysis |

## infra/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_CITUS.md](infra/PLAN_CITUS.md) | PLAN | — | Citus distributed table compatibility |
| [PLAN_CODECOV.md](infra/PLAN_CODECOV.md) | PLAN | Implementing | Codecov integration for coverage reporting |
| [PLAN_GITHUB_ACTIONS_COST.md](infra/PLAN_GITHUB_ACTIONS_COST.md) | PLAN | — | Reduce GitHub Actions resource consumption |
| [PLAN_DOCKER_IMAGE.md](infra/PLAN_DOCKER_IMAGE.md) | PLAN | Draft | Official Docker image |
| [PLAN_EXTERNAL_PROCESS.md](infra/PLAN_EXTERNAL_PROCESS.md) | REPORT | Exploration | External sidecar process feasibility study |
| [PLAN_MULTI_DATABASE.md](infra/PLAN_MULTI_DATABASE.md) | PLAN | Draft | Multi-database support |
| [PLAN_PACKAGING.md](infra/PLAN_PACKAGING.md) | PLAN | Draft | Distribution packaging |
| [PLAN_PG19_COMPAT.md](infra/PLAN_PG19_COMPAT.md) | PLAN | Draft | PostgreSQL 19 forward-compatibility |
| [PLAN_PGWIRE_PROXY.md](infra/PLAN_PGWIRE_PROXY.md) | REPORT | Research | pgwire proxy / intercept analysis |
| [PLAN_PG_BACKCOMPAT.md](infra/PLAN_PG_BACKCOMPAT.md) | REPORT | Research | Supporting older PostgreSQL versions (13–17) |
| [PLAN_VERSIONING.md](infra/PLAN_VERSIONING.md) | PLAN | Draft | Semantic versioning & compatibility policy |

## performance/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_PERFORMANCE_PART_8.md](performance/PLAN_PERFORMANCE_PART_8.md) | PLAN | — | Residual bottlenecks & next-wave optimizations |
| [PLAN_PERFORMANCE_PART_9.md](performance/PLAN_PERFORMANCE_PART_9.md) | PLAN | Planning | Strategic performance roadmap |
| [REPORT_PARALLELIZATION.md](performance/REPORT_PARALLELIZATION.md) | REPORT | Planning | Parallelization options analysis |
| [STATUS_PERFORMANCE.md](performance/STATUS_PERFORMANCE.md) | STATUS | — | Performance benchmark history & trends |
| [PLAN_TRIGGERS_OVERHEAD.md](performance/PLAN_TRIGGERS_OVERHEAD.md) | PLAN | — | CDC trigger write-side overhead benchmark |

## sql/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_CIRCULAR_REFERENCES.md](sql/PLAN_CIRCULAR_REFERENCES.md) | PLAN | Not started | Circular references in the dependency graph |
| [PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](sql/PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md) | PLAN | Proposed | Multi-path refresh for diamond dependencies in the DAG |
| [PLAN_LATERAL_JOINS.md](sql/PLAN_LATERAL_JOINS.md) | PLAN | Implemented | LATERAL join support (subqueries with LATERAL) |
| [PLAN_NON_DETERMINISM.md](sql/PLAN_NON_DETERMINISM.md) | PLAN | Not started | Non-deterministic function handling |
| [PLAN_DB_SCHEMA_STABILITY.md](sql/PLAN_DB_SCHEMA_STABILITY.md) | REPORT | Assessment | Database schema stability assessment (pre-1.0) |
| [PLAN_HYBRID_CDC.md](sql/PLAN_HYBRID_CDC.md) | PLAN | Complete | Hybrid CDC — trigger bootstrap → logical replication |
| [PLAN_NATIVE_SYNTAX.md](sql/PLAN_NATIVE_SYNTAX.md) | PLAN | Proposed | Native PostgreSQL syntax for stream tables |
| [PLAN_STREAMING_AGGREGATION.md](sql/PLAN_STREAMING_AGGREGATION.md) | PLAN | Draft | Sub-second latency path via streaming aggregation |
| [PLAN_TRANSACTIONAL_IVM.md](sql/PLAN_TRANSACTIONAL_IVM.md) | PLAN | Proposed | Transactionally updated views (immediate IVM) |
| [PLAN_UPGRADE_MIGRATIONS.md](sql/PLAN_UPGRADE_MIGRATIONS.md) | PLAN | Draft | Extension upgrade migrations |
| [PLAN_USER_TRIGGERS_EXPLICIT_DML.md](sql/PLAN_USER_TRIGGERS_EXPLICIT_DML.md) | PLAN | Implemented | User triggers on stream tables via explicit DML |
| [PLAN_VIEW_INLINING.md](sql/PLAN_VIEW_INLINING.md) | PLAN | Implemented | View inlining for stream tables |
| [GAP_SQL_OVERVIEW.md](sql/GAP_SQL_OVERVIEW.md) | GAP | Reference | SQL support gap analysis (periodically updated) |
| [REPORT_TRIGGERS_VS_REPLICATION.md](sql/REPORT_TRIGGERS_VS_REPLICATION.md) | REPORT | Reference | Triggers vs logical replication for CDC |
| [GAP_SQL_PHASE_4.md](sql/GAP_SQL_PHASE_4.md) | GAP | Complete | SQL gaps — phase 4 |
| [GAP_SQL_PHASE_5.md](sql/GAP_SQL_PHASE_5.md) | GAP | In progress | SQL gaps — phase 5 |
| [GAP_SQL_PHASE_6.md](sql/GAP_SQL_PHASE_6.md) | GAP | Reference | SQL gaps — phase 6 (comprehensive analysis) |
| [GAP_SQL_PHASE_7.md](sql/GAP_SQL_PHASE_7.md) | GAP | In progress | SQL gaps — phase 7 (deep analysis) |
| [GAP_SQL_PHASE_7_QUESTIONS.md](sql/GAP_SQL_PHASE_7_QUESTIONS.md) | GAP | — | Open questions from GAP_SQL_PHASE_7 |

## testing/

| File | Type | Status | Summary |
|------|------|--------|---------|
| [PLAN_TEST_SUITES.md](testing/PLAN_TEST_SUITES.md) | PLAN | Proposed | External test suites for pg_trickle |
| [PLAN_TEST_SUITE_TPC_H.md](testing/PLAN_TEST_SUITE_TPC_H.md) | PLAN | In progress | TPC-H test suite |
| [STATUS_TESTING.md](testing/STATUS_TESTING.md) | STATUS | — | Testing & coverage status |
