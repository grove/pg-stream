# Plan: Architecture Decision Records

Date: 2026-02-24
Status: PROPOSED

---

## Overview

This plan proposes a set of Architecture Decision Records (ADRs) to document the
significant technical choices made during pg_stream development. One ADR already
exists — [ADR-001 Triggers Instead of Logical Replication](adr-triggers-instead-of-logical-replication.md).
The remaining decisions are currently scattered across PLAN files, reports, code
comments, and commit history.

### ADR Format

Each ADR follows a standard template:

```markdown
# ADR-NNN: <Title>

| Field         | Value                   |
|---------------|-------------------------|
| **Status**    | Accepted / Superseded / Proposed |
| **Date**      | YYYY-MM-DD              |
| **Deciders**  | pg_stream core team     |
| **Category**  | <area>                  |

## Context
## Decision
## Options Considered
## Consequences
## References
```

### Numbering Convention

- **ADR-001–009**: Core architecture (CDC, IVM engine, storage)
- **ADR-010–019**: API & schema design
- **ADR-020–029**: Scheduling & runtime
- **ADR-030–039**: Tooling, testing, ecosystem
- **ADR-040–049**: Performance & optimization

---

## Proposed ADRs

### ADR-001: Row-Level Triggers Instead of Logical Replication for CDC

| Field | Value |
|-------|-------|
| **Status** | Accepted (already written) |
| **File** | `adr-triggers-instead-of-logical-replication.md` |
| **Category** | CDC |

Already exists. The foundational CDC decision: triggers for single-transaction
atomicity at creation time, avoiding the `pg_create_logical_replication_slot()`
write-context restriction.

---

### ADR-002: Hybrid CDC — Trigger Bootstrap with WAL Steady-State

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | CDC |
| **Sources** | `plans/sql/PLAN_HYBRID_CDC.md`, `plans/sql/REPORT_TRIGGERS_VS_REPLICATION.md` |

**Decision:** After ADR-001 chose triggers, implement a hybrid approach: use
triggers at creation time (zero-config, atomic), then transparently transition
to logical replication for steady-state if `wal_level = logical`.

**Key points to document:**
- Three CDC states: TRIGGER → TRANSITIONING → WAL
- No-data-loss transition (trigger stays active until WAL catches up)
- Graceful fallback if slot creation fails
- Same buffer table schema regardless of CDC mode
- `pg_stream.cdc_mode` GUC for user control (auto/trigger/wal)

---

### ADR-003: Query Differentiation via Operator Tree (DVM Engine Design)

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | IVM Engine |
| **Sources** | `plans/PLAN.md` Phase 6, `docs/DVM_OPERATORS.md`, `docs/ARCHITECTURE.md` |

**Decision:** Implement incremental view maintenance by parsing the defining
query into an operator tree and applying per-operator differentiation rules
(analogous to automatic differentiation in calculus) to generate delta SQL.

**Alternatives to document:**
- Full recomputation only (simple but O(n) always)
- Log-based delta replay (simpler operators, less SQL coverage)
- DBSP-style Z-sets with explicit multiplicity tracking
- pg_ivm's approach (limited to single-table aggregates at the time)

**Key points:**
- Phased operator support: Phase 1 (scan, filter, project, inner join,
  aggregate, distinct) → Phase 2 (outer joins, UNION ALL, window, LATERAL,
  recursive CTE, INTERSECT, EXCEPT)
- Delta SQL is generated as CTEs, not materialized intermediates
- Row identity via `__pgs_row_id` (xxHash) for diff-based delta application
- Theoretical basis: DBSP (Budiu et al. 2023), Gupta & Mumick (1995)

---

### ADR-004: xxHash Row IDs Instead of UUIDs

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Storage / IVM Engine |
| **Sources** | `plans/PLAN.md` Key Design Decisions, `src/hash.rs`, `src/dvm/row_id.rs` |

**Decision:** Use 64-bit xxHash of the primary key as the `__pgs_row_id`
column (stored as `BIGINT`) rather than UUIDs or composite-key matching.

**Alternatives to document:**
- UUID v4 (128-bit, zero collision, 16 bytes per row)
- Composite primary key matching (no extra column, but complex MERGE logic)
- MD5/SHA hash (cryptographically stronger but slower)

**Key points:**
- 8 bytes vs 16 bytes per row (significant at scale)
- Collision probability: ~1 in 2^64 per unique key — acceptable for practical datasets
- `pg_stream_hash()` for single-column PKs, `pg_stream_hash_multi()` for composites
- Visible to users via `SELECT *` — a known tradeoff (see ADR-010)

---

### ADR-005: Per-Table Change Buffer Tables Instead of In-Memory Queues

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | CDC / Storage |
| **Sources** | `plans/PLAN.md` Key Design Decisions, `src/cdc.rs` |

**Decision:** Store CDC changes in dedicated PostgreSQL tables
(`pgstream_changes.changes_<oid>`) rather than in shared memory, message
queues, or a single global changes table.

**Alternatives to document:**
- Shared memory ring buffer (fast, but limited size, not crash-safe)
- Single global changes table (simpler, but contention on high-write workloads)
- External message queue (Kafka, NATS — unnecessary dependency)

**Key points:**
- Crash-safe: survives backend/worker crashes
- Queryable for debugging and monitoring
- Per-table isolation avoids contention across independent source tables
- Aggressive cleanup after each refresh cycle
- Trade-off: extra I/O vs. durability and simplicity

---

### ADR-006: Explicit DML for User Triggers Instead of MERGE Decomposition at All Times

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Refresh Engine |
| **Sources** | `plans/sql/PLAN_USER_TRIGGERS_EXPLICIT_DML.md` |

**Decision:** When a stream table has user-defined triggers, decompose the
MERGE into three explicit DML statements (DELETE, UPDATE, INSERT) so triggers
fire with correct `TG_OP`, `OLD`, and `NEW`. When no user triggers exist,
keep the fast single-MERGE path.

**Alternatives to document:**
- Always use explicit DML (simpler code, but ~10-30% slower for the common case)
- Always use MERGE + replay triggers after (complex, wrong `TG_OP` context)
- Disallow user triggers on stream tables entirely

**Key points:**
- `has_user_triggers()` detection at refresh time
- `CachedMergeTemplate` extended with explicit DML templates
- `pg_stream.user_triggers` GUC (auto/on/off)
- FULL refresh: triggers suppressed via `DISABLE TRIGGER USER` + `NOTIFY`
- Phase 2 (FULL refresh trigger support via snapshot-diff replay) rejected as
  too complex for marginal benefit

---

### ADR-010: SQL Functions Instead of DDL Syntax

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | API Design |
| **Sources** | `plans/PLAN.md` Key Design Decisions |

**Decision:** Expose the API as SQL functions (`pgstream.create_stream_table()`,
etc.) rather than custom DDL syntax (`CREATE STREAM TABLE ...`).

**Alternatives to document:**
- Custom DDL via PostgreSQL parser hooks or grammar extension (native feel, but
  requires maintaining a fork or complex parser plugin)
- Foreign Data Wrapper interface (misuse of the FDW protocol)
- Hook-based interception of `CREATE MATERIALIZED VIEW`

**Key points:**
- Works without PostgreSQL parser modifications
- Clean extension boundary — standard `CREATE EXTENSION` installation
- Idiomatic PostgreSQL extension pattern
- Trade-off: less "native" feel, no `\d`-style psql integration

---

### ADR-011: `pgstream` Schema with `pgs_` Prefix Convention

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | API Design / Naming |
| **Sources** | Code history (dt_ → st_ → pgs_ rename across 72 files) |

**Decision:** All internal catalog objects use the `pgstream` schema and `pgs_`
column/table prefix. Change buffers live in a separate `pgstream_changes` schema.

**Key points to document:**
- Original naming used `dt_` (derived table), renamed to `st_` (stream table),
  then to `pgs_` (pg_stream) for global uniqueness and consistency
- Two schemas: `pgstream` (API + catalog) and `pgstream_changes` (buffer tables)
- `pgs_` prefix avoids collisions with user objects
- Cost of the rename: 72 files, 872 test assertions updated

---

### ADR-012: PostgreSQL 18 as Sole Target

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | API Design / Platform |
| **Sources** | `plans/PLAN.md` Key Design Decisions |

**Decision:** Target PostgreSQL 18 exclusively. No backward compatibility with
PG 16 or PG 17.

**Alternatives to document:**
- Multi-version support via conditional compilation (broader adoption, much
  higher maintenance)
- Target PG 17 as minimum (more users, but miss PG 18 features)

**Key points:**
- PG 18 features used: custom cumulative statistics, improved logical
  replication, DSM improvements
- Narrows user base but simplifies development and testing
- pgrx 0.17.x provides PG 18 support

---

### ADR-020: Canonical Scheduling Periods (48·2ⁿ Seconds)

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Scheduling |
| **Sources** | `plans/PLAN.md` Key Design Decisions, `src/scheduler.rs` |

**Decision:** Use a discrete set of canonical refresh periods (48, 96, 192, ...
seconds) rather than arbitrary user-specified intervals.

**Alternatives to document:**
- Exact user-specified intervals (precise but causes timestamp drift across STs)
- Fixed grid (e.g., every minute — inflexible)
- Event-driven (refresh only when changes exist — complex, unpredictable latency)

**Key points:**
- Guarantees `data_timestamp` alignment across stream tables with different
  schedules in the same DAG
- User-specified schedule is snapped to the nearest (smaller) canonical period
- NULL schedule = DOWNSTREAM (refresh only when triggered by a dependent)
- Advisory locks prevent concurrent refreshes of the same ST

---

### ADR-021: Single Background Worker Scheduler

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Scheduling / Runtime |
| **Sources** | `src/scheduler.rs`, `src/shmem.rs`, `docs/ARCHITECTURE.md` |

**Decision:** Use a single background worker for scheduling, with shared memory
for inter-process communication (`PgLwLock<PgStreamSharedState>` and
`PgAtomic<AtomicU64>` DAG rebuild signal).

**Alternatives to document:**
- Multiple workers (one per stream table — complex coordination, resource waste)
- No background worker — rely on user-initiated refreshes only
- External scheduler (cron, pg_cron — loses atomicity with extension state)

**Key points:**
- Wakes at `pg_stream.scheduler_interval_ms` intervals
- Detects DAG changes via atomic counter comparison (lock-free)
- Topological refresh ordering within each wake cycle
- `SIGTERM` graceful shutdown
- `pg_stream.enabled` GUC to disable without unloading

---

### ADR-022: Replication Origin for Feedback Loop Prevention

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Refresh Engine |
| **Sources** | `plans/PLAN.md` Key Design Decisions, `src/refresh.rs` |

**Decision:** Use PostgreSQL's replication origin mechanism
(`pg_stream_refresh`) to tag refresh-generated writes, preventing CDC triggers
from re-capturing changes made by the refresh itself (feedback loops).

**Alternatives to document:**
- Session-level GUC flag checked in trigger function
- `session_replication_role = replica` (disables all triggers, too broad)
- Separate "shadow" tables for internal writes

**Key points:**
- Standard PostgreSQL mechanism (`pg_replication_origin_session_setup`)
- Reliable filtering in the trigger function
- No user-visible side effects

---

### ADR-023: Adaptive Full-Refresh Fallback

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Refresh Engine / Performance |
| **Sources** | `docs/ARCHITECTURE.md`, `src/refresh.rs` |

**Decision:** When the change ratio exceeds
`pg_stream.differential_max_change_ratio`, automatically downgrade a
DIFFERENTIAL refresh to FULL, since delta processing becomes more expensive
than full recomputation at high change rates.

**Alternatives to document:**
- Always DIFFERENTIAL (degrades badly at high change rates — see benchmarks)
- Always FULL (simple but misses the core incremental advantage)
- User-specified per-table override only

**Key points:**
- Benchmarks show DIFFERENTIAL is slower than FULL at ~50% change rate
- Automatic switching keeps the default experience fast
- Per-stream-table `auto_threshold` in catalog allows tuning
- `last_full_ms` tracks full-refresh cost for adaptive comparison

---

### ADR-030: dbt Integration via Macro Package (Not Custom Adapter)

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Ecosystem / Tooling |
| **Sources** | `plans/dbt/PLAN_DBT_MACRO.md`, `plans/dbt/PLAN_DBT_ADAPTER.md` |

**Decision:** Integrate with dbt via a Jinja macro package with a custom
`stream_table` materialization, using the standard `dbt-postgres` adapter.
Defer the full custom Python adapter (Option B) as an upgrade path.

**Alternatives to document:**
- Full custom adapter (`dbt-pgstream` Python package) — hides `__pgs_row_id`,
  custom relation type, native `dbt source freshness`, but ~54 hours effort
- No dbt integration — simplest, but alienates dbt users

**Key points:**
- ~15 hours effort (vs ~54 for adapter)
- No Python code — pure Jinja SQL macros
- Works with dbt Core ≥ 1.6 (for `subdirectory` in `packages.yml`)
- Trade-off: `__pgs_row_id` visible in docs, no custom relation type
- Adapter plan exists as documented upgrade path

---

### ADR-031: dbt Package In-Repo (Subdirectory) Instead of Separate Repository

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Ecosystem / Tooling |
| **Sources** | `plans/dbt/PLAN_DBT_MACRO.md`, `plans/ecosystem/PLAN_ECO_SYSTEM.md` |

**Decision:** Ship the dbt macro package as `dbt-pgstream/` inside the main
pg_stream repository, not in a separate repo.

**Alternatives to document:**
- Separate repository (independent release cadence, cleaner separation)
- dbt Hub package (requires Hub submission, separate repo)
- npm/pip-style package (wrong ecosystem)

**Key points:**
- SQL API changes in the Rust extension are immediately validated against macros
  in the same PR (via CI `dbt-integration` job)
- Simpler contributor workflow — one repo, one PR
- Users install via `git:` + `subdirectory:` in `packages.yml`
- Trade-off: shared git tags, dbt-only fixes require extension release tag
- Extractable to separate repo later if tag coupling becomes a problem

---

### ADR-032: Testcontainers-Based Integration Testing (No Local PG Dependency)

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | Testing |
| **Sources** | `AGENTS.md`, `plans/testing/STATUS_TESTING.md`, `tests/common/mod.rs` |

**Decision:** All integration and E2E tests use Docker containers via
testcontainers-rs and a custom E2E Docker image. Tests never assume a local
PostgreSQL installation.

**Alternatives to document:**
- Local PG installation (faster startup, but fragile and environment-dependent)
- pgrx-managed PG only (limited — only unit-level tests)
- Cloud-hosted test databases (slow, costly, flaky network)

**Key points:**
- Custom `Dockerfile.e2e` builds PG 18 + pg_stream from source
- Deterministic, reproducible test environments
- Three-tier test pyramid: unit (no DB) → integration (testcontainers) → E2E
  (full extension Docker image)
- `#[tokio::test]` for all async test tiers

---

### ADR-040: Aggregate Maintenance via Auxiliary Counter Columns

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | IVM Engine / Performance |
| **Sources** | `plans/PLAN.md` Key Design Decisions, `docs/DVM_OPERATORS.md`, `src/dvm/operators/aggregate.rs` |

**Decision:** Maintain aggregates incrementally by storing auxiliary counter
columns alongside each aggregate result (similar to pg_ivm's approach).

**Alternatives to document:**
- Full group recomputation on any change (simple but O(group_size))
- Z-set multiplicity tracking (DBSP-style — more general but complex)
- Partial aggregation with checkpointing (streaming systems approach)

**Key points:**
- `COUNT(*)` maintained via `__pgs_count` counter
- `SUM(x)` maintained via `__pgs_sum_x` + `__pgs_count` for correctness when
  group shrinks to zero
- `AVG(x)` derived from SUM/COUNT at read time
- `MIN/MAX` degrade gracefully (require rescan when min/max row is deleted)
- Hidden auxiliary columns increase storage but enable O(1) aggregate updates

---

### ADR-041: LATERAL Subquery Diff via Row-Scoped Recomputation

| Field | Value |
|-------|-------|
| **Status** | To write |
| **Category** | IVM Engine |
| **Sources** | `plans/sql/LATERAL_JOINS.md`, `src/dvm/operators/lateral_function.rs` |

**Decision:** Differentiate LATERAL subqueries (and SRFs in FROM) by
**row-scoped recomputation**: when an outer row changes, re-execute the
correlated subquery for that specific row only.

**Alternatives to document:**
- Full recomputation of all LATERAL outputs (correct but O(n))
- Decorrelation + standard join diff (only works for a subset of LATERAL queries)
- Unsupported / reject LATERAL queries

**Key points:**
- Reuses the same strategy as `LateralFunction` (SRFs in FROM)
- Handles both implicit LATERAL (comma-syntax) and explicit `LEFT JOIN LATERAL`
- Supports top-N per group, correlated aggregation, multi-column derived values
- Correctness relies on re-executing the subquery in the context of the changed
  outer row — not on incremental maintenance of the inner query

---

## Priority Order

| Priority | ADR | Rationale |
|----------|-----|-----------|
| 1 | ADR-001 (exists) | Foundational CDC decision, already documented |
| 2 | ADR-003 | Core IVM engine design — the heart of the extension |
| 3 | ADR-010 | SQL functions vs DDL — shapes the entire user experience |
| 4 | ADR-002 | Hybrid CDC — major architectural evolution from ADR-001 |
| 5 | ADR-004 | xxHash row IDs — affects storage, correctness, user visibility |
| 6 | ADR-005 | Change buffer design — fundamental to CDC pipeline |
| 7 | ADR-020 | Canonical scheduling — non-obvious design choice |
| 8 | ADR-030 | dbt macro vs adapter — recent, high-impact ecosystem decision |
| 9 | ADR-006 | User triggers — important for real-world usability |
| 10 | ADR-012 | PG 18 only — scoping decision with broad implications |
| 11 | ADR-040 | Aggregate maintenance — key IVM correctness/performance choice |
| 12 | ADR-022 | Replication origin — clever but non-obvious safety mechanism |
| 13 | ADR-023 | Adaptive fallback — affects default performance characteristics |
| 14 | ADR-021 | Single scheduler — straightforward but worth documenting |
| 15 | ADR-011 | Naming convention — mostly historical, low urgency |
| 16 | ADR-031 | In-repo dbt package — minor but worth recording |
| 17 | ADR-032 | Testcontainers — testing infrastructure choice |
| 18 | ADR-041 | LATERAL diff strategy — specialized IVM detail |

---

## Effort Estimate

| Batch | ADRs | Estimated Effort |
|-------|------|------------------|
| Batch 1 — Core | ADR-002, 003, 004, 005, 010 | ~4 hours |
| Batch 2 — Runtime | ADR-006, 012, 020, 021, 022, 023 | ~3 hours |
| Batch 3 — Ecosystem | ADR-030, 031, 032 | ~2 hours |
| Batch 4 — IVM Details | ADR-040, 041 | ~1.5 hours |
| **Total** | **17 new ADRs** | **~10.5 hours** |

---

## File Naming Convention

```
plans/adrs/
├── PLAN_ADRS.md                                              ← this file
├── adr-triggers-instead-of-logical-replication.md            ← ADR-001 (existing)
├── adr-002-hybrid-cdc.md
├── adr-003-dvm-operator-tree.md
├── adr-004-xxhash-row-ids.md
├── adr-005-per-table-change-buffers.md
├── adr-006-explicit-dml-user-triggers.md
├── adr-010-sql-functions-not-ddl.md
├── adr-011-pgstream-schema-naming.md
├── adr-012-postgresql-18-only.md
├── adr-020-canonical-scheduling-periods.md
├── adr-021-single-background-worker.md
├── adr-022-replication-origin-feedback-prevention.md
├── adr-023-adaptive-full-refresh-fallback.md
├── adr-030-dbt-macro-package.md
├── adr-031-dbt-in-repo-subdirectory.md
├── adr-032-testcontainers-testing.md
├── adr-040-aggregate-auxiliary-counters.md
└── adr-041-lateral-row-scoped-recomputation.md
```
