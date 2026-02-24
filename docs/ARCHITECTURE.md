# Architecture

This document describes the internal architecture of pg_stream — a PostgreSQL 18 extension that implements stream tables with differential view maintenance.

---

## High-Level Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                     PostgreSQL 18 Backend                       │
│                                                                 │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌─────────────┐   │
│  │  Source  │   │  Source  │   │  Storage │   │  Storage    │   │
│  │  Table A │   │  Table B │   │  Table X │   │  Table Y    │   │
│  └────┬─────┘   └────┬─────┘   └────▲─────┘   └────▲────────┘   │
│       │              │              │              │            │
│  ═════╪══════════════╪══════════════╪══════════════╪════════    │
│       │              │              │              │            │
│  ┌────▼──────────────▼────┐   ┌────┴──────────────┴────┐        │
│  │  Hybrid CDC Layer      │   │  Delta Application     │        │
│  │  Triggers ──or── WAL   │   │  (INSERT/DELETE diffs) │        │
│  └────────────┬───────────┘   └────────────▲───────────┘        │
│               │                            │                    │
│  ┌────────────▼───────────┐   ┌────────────┴───────────┐        │
│  │   Change Buffer        │   │   DVM Engine           │        │
│  │   (pgstream_changes.*) │   │   (Operator Tree)      │        │
│  └────────────┬───────────┘   └────────────▲───────────┘        │
│               │                            │                    │
│               └────────────┬───────────────┘                    │
│                            │                                    │
│  ┌─────────────────────────▼─────────────────────────────┐      │
│  │              Refresh Engine                           │      │
│  │  ┌──────────┐  ┌──────────┐  ┌─────────────────────┐  │      │
│  │  │ Frontier │  │ DAG      │  │ Scheduler           │  │      │
│  │  │ Tracker  │  │ Resolver │  │ (canonical schedule)│  │      │
│  │  └──────────┘  └──────────┘  └─────────────────────┘  │      │
│  └───────────────────────────────────────────────────────┘      │
│                                                                 │
│  ┌────────────────────────────────────────────────────────┐     │
│  │                    Catalog (pgstream.*)                │     │
│  │  pgs_stream_tables │ pgs_dependencies │ pgs_refresh_history│  │
│  └────────────────────────────────────────────────────────┘     │
│                                                                 │
│  ┌──────────────────────────────────────────────────────┐       │
│  │                  Monitoring Layer                    │       │
│  │  st_refresh_stats │ slot_health │ check_cdc_health    │       │
│  │  explain_st │ views │ NOTIFY alerting               │       │
│  └──────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────┘
```

---

## Component Details

### 1. SQL API Layer (`src/api.rs`)

The public entry point for users. All operations are exposed as `#[pg_extern]` functions in the `pgstream` schema:

- **create_stream_table** — Parses the defining query, builds an operator tree, creates the storage table, registers CDC slots, populates the catalog, and optionally performs an initial full refresh.
- **alter_stream_table** — Modifies schedule, refresh mode, or status (ACTIVE/SUSPENDED).
- **drop_stream_table** — Removes the storage table, catalog entries, and cleans up CDC slots.
- **refresh_stream_table** — Triggers a manual refresh (same path as automatic scheduling).
- **pgs_status** — Returns a summary of all registered stream tables.

### 2. Catalog (`src/catalog.rs`)

The catalog manages persistent metadata stored in PostgreSQL tables within the `pgstream` schema:

| Table | Purpose |
|---|---|
| `pgstream.pgs_stream_tables` | Core metadata: name, query, schedule, status, frontier, etc. |
| `pgstream.pgs_dependencies` | DAG edges from ST to source tables |
| `pgstream.pgs_refresh_history` | Audit log of every refresh operation |
| `pgstream.pgs_change_tracking` | Per-source CDC slot metadata |

Schema creation is handled by `extension_sql!()` macros that run at `CREATE EXTENSION` time.

#### Entity-Relationship Diagram

```mermaid
erDiagram
    pgs_stream_tables {
        bigserial pgs_id PK
        oid pgs_relid UK "OID of materialized storage table"
        text pgs_name
        text pgs_schema
        text defining_query
        text schedule "Duration or cron expression"
        text refresh_mode "FULL | DIFFERENTIAL"
        text status "INITIALIZING | ACTIVE | SUSPENDED | ERROR"
        boolean is_populated
        timestamptz data_timestamp "Freshness watermark"
        jsonb frontier "DBSP-style version frontier"
        timestamptz last_refresh_at
        int consecutive_errors
        boolean needs_reinit
        float8 auto_threshold
        float8 last_full_ms
        timestamptz created_at
        timestamptz updated_at
    }

    pgs_dependencies {
        bigint pgs_id PK,FK "References pgs_stream_tables.pgs_id"
        oid source_relid PK "OID of source table"
        text source_type "TABLE | STREAM_TABLE | VIEW"
        text_arr columns_used "Column-level lineage"
        text cdc_mode "TRIGGER | TRANSITIONING | WAL"
        text slot_name "Replication slot (WAL mode)"
        pg_lsn decoder_confirmed_lsn "WAL decoder progress"
        timestamptz transition_started_at "Trigger→WAL transition start"
    }

    pgs_refresh_history {
        bigserial refresh_id PK
        bigint pgs_id FK "References pgs_stream_tables.pgs_id"
        timestamptz data_timestamp
        timestamptz start_time
        timestamptz end_time
        text action "NO_DATA | FULL | DIFFERENTIAL | REINITIALIZE | SKIP"
        bigint rows_inserted
        bigint rows_deleted
        text error_message
        text status "RUNNING | COMPLETED | FAILED | SKIPPED"
        text initiated_by "SCHEDULER | MANUAL | INITIAL"
        timestamptz freshness_deadline
    }

    pgs_change_tracking {
        oid source_relid PK "OID of tracked source table"
        text slot_name "Trigger function name"
        pg_lsn last_consumed_lsn
        bigint_arr tracked_by_pgs_ids "ST IDs sharing this source"
    }

    pgs_stream_tables ||--o{ pgs_dependencies : "has sources"
    pgs_stream_tables ||--o{ pgs_refresh_history : "has refresh history"
    pgs_stream_tables }o--o{ pgs_change_tracking : "tracks via pgs_ids array"
```

> **Note:** Change buffer tables (`pgstream_changes.changes_<oid>`) are created dynamically per source table OID and live in the separate `pgstream_changes` schema.

### 3. CDC / Change Data Capture (`src/cdc.rs`, `src/wal_decoder.rs`)

pg_stream uses a **hybrid CDC** architecture that starts with triggers and optionally transitions to WAL-based (logical replication) capture for lower write-side overhead.

#### Trigger Mode (default)

1. **Trigger Management** — Creates `AFTER INSERT OR UPDATE OR DELETE` row-level triggers (`pg_stream_cdc_<oid>`) on each tracked source table. Each trigger fires a PL/pgSQL function (`pg_stream_cdc_fn_<oid>()`) that writes changes to the buffer table.
2. **Change Buffering** — Decoded changes are written to per-source change buffer tables in the `pgstream_changes` schema. Each row captures the LSN (`pg_current_wal_lsn()`), transaction ID, action type (I/U/D), and the new/old row data as JSONB via `to_jsonb()`.
3. **Cleanup** — Consumed changes are deleted after each successful refresh via `delete_consumed_changes()`, bounded by the upper LSN to prevent unbounded scans.
4. **Lifecycle** — Triggers and trigger functions are automatically created when a source table is first tracked and dropped when the last stream table referencing a source is removed.

The trigger approach was chosen as the default for **transaction safety** (triggers can be created in the same transaction as DDL), **simplicity** (no slot management, no `wal_level = logical` requirement), and **immediate visibility** (changes are visible in buffer tables as soon as the source transaction commits).

#### WAL Mode (optional, automatic transition)

When `pg_stream.cdc_mode` is set to `'auto'` or `'wal'` and `wal_level = logical` is available, the system transitions from trigger-based to WAL-based CDC after the first successful refresh:

1. **WAL Availability Detection** — At stream table creation, checks whether `wal_level = logical` is configured. If so, the source dependency is marked for WAL transition.
2. **WAL Decoder Background Worker** — A dedicated background worker (`src/wal_decoder.rs`) polls logical replication slots and writes decoded changes into the same change buffer tables used by triggers, ensuring a uniform format for the DVM engine.
3. **Transition Orchestration** — The transition is a three-step process: (a) create a replication slot, (b) wait for the decoder to catch up to the trigger's last confirmed LSN, (c) drop the trigger and switch the dependency to WAL mode. If the decoder doesn't catch up within `pg_stream.wal_transition_timeout` (default 300s), the system falls back to triggers.
4. **CDC Mode Tracking** — Each source dependency in `pgs_dependencies` carries a `cdc_mode` column (TRIGGER / TRANSITIONING / WAL) and WAL-specific metadata (`slot_name`, `decoder_confirmed_lsn`, `transition_started_at`).

See ADR-001 and ADR-002 in [plans/adrs/PLAN_ADRS.md](../plans/adrs/PLAN_ADRS.md) for the original design rationale and [plans/sql/PLAN_HYBRID_CDC.md](../plans/sql/PLAN_HYBRID_CDC.md) for the full implementation plan.

### 4. DVM Engine (`src/dvm/`)

The Differential View Maintenance engine is the core of the system. It transforms the defining SQL query into an executable operator tree that can compute deltas efficiently.

#### Query Parser (`src/dvm/parser.rs`)

Parses the defining query using PostgreSQL's internal parser (via pgrx `raw_parser`) and extracts:
- **WITH clause** — CTE definitions (non-recursive: inline expansion or shared delta; recursive: detected for mode gating)
- **Target list** — output columns
- **FROM clause** — source tables, joins, subqueries, and CTE references
- **WHERE clause** — filters
- **GROUP BY / aggregate functions**
- **DISTINCT / UNION ALL / INTERSECT / EXCEPT**

The parser produces an `OpTree` — a tree of operator nodes. CTE handling follows a tiered approach:

1. **Tier 1 (Inline Expansion)** — Non-recursive CTEs referenced once are expanded into `Subquery` nodes, equivalent to subqueries in FROM.
2. **Tier 2 (Shared Delta)** — Non-recursive CTEs referenced multiple times produce `CteScan` nodes that share a single delta computation via a CTE registry and delta cache.
3. **Tier 3a/3b (Recursive)** — Recursive CTEs (`WITH RECURSIVE`) are detected via `query_has_recursive_cte()`. In FULL mode, the query executes as-is. In DIFFERENTIAL mode, a recomputation diff strategy re-executes the full query and anti-joins against storage.

#### Operators (`src/dvm/operators/`)

Each operator knows how to generate a **delta query** — given a set of changes to its inputs, it produces the corresponding changes to its output:

| Operator | Delta Strategy |
|---|---|
| **Scan** | Direct passthrough of CDC changes |
| **Filter** | Apply WHERE predicate to deltas |
| **Project** | Apply column projection to deltas |
| **Join** | Join deltas against the other side's current state |
| **OuterJoin** | LEFT/RIGHT/FULL outer join with NULL padding |
| **Aggregate** | Recompute group values where affected keys changed |
| **Distinct** | COUNT-based duplicate tracking |
| **UnionAll** | Merge deltas from both branches |
| **Intersect** | Dual-count multiplicity with LEAST boundary crossing |
| **Except** | Dual-count multiplicity with GREATEST(0, L-R) boundary crossing |
| **Subquery** | Transparent delegation + optional column renaming (CTEs, subselects) |
| **CteScan** | Shared delta lookup from CTE cache (multi-reference CTEs) |
| **Window** | Partition-based recomputation for window functions |
| **LateralFunction** | Row-scoped recomputation for SRFs in FROM (jsonb_array_elements, unnest, etc.) |

See [DVM_OPERATORS.md](DVM_OPERATORS.md) for detailed descriptions.

#### Diff Engine (`src/dvm/diff.rs`)

Generates the final diff SQL that:
1. Computes the delta from the operator tree
2. Produces `('+', row)` for inserts and `('-', row)` for deletes
3. Applies the diff via `DELETE` matching old rows and `INSERT` for new rows

### 5. DAG / Dependency Graph (`src/dag.rs`)

Stream tables can depend on other stream tables (cascading), forming a Directed Acyclic Graph:

- **Cycle detection** — Prevents circular dependencies at creation time using DFS.
- **Topological ordering** — Determines refresh order: upstream STs must be refreshed before downstream STs.
- **Cascade operations** — When a source table changes, all transitive dependents are identified for refresh.

### 6. Version / Frontier Tracking (`src/version.rs`)

Implements a per-source **frontier** (JSONB map of `source_oid → LSN`) to track exactly how far each stream table has consumed changes:

- **Read frontier** — Before refresh, read the frontier to know where to start consuming changes.
- **Advance frontier** — After a successful refresh, the frontier is updated to the latest consumed LSN.
- **Consistent snapshots** — The frontier ensures that each refresh processes a contiguous, non-overlapping window of changes.

#### Delayed View Semantics (DVS) Guarantee

The contents of every stream table are logically equivalent to evaluating its defining query at some past point in time — the `data_timestamp`. The scheduler refreshes STs in **topological order** so that when ST B references upstream ST A, A has already been refreshed to the target `data_timestamp` before B runs its delta query against A's contents. The frontier lifecycle is:

1. **Created** — on first full refresh; records the LSN of each source at that moment.
2. **Advanced** — on each differential refresh; the old frontier becomes the lower bound and the new frontier (with fresh LSNs) the upper bound. The DVM engine reads changes in `[old, new]`.
3. **Reset** — on reinitialize; a fresh frontier is created from scratch.

### 7. Refresh Engine (`src/refresh.rs`)

Orchestrates the complete refresh cycle:

```
┌──────────────┐
│  Check State │ → Is ST active? Has it been populated?
└──────┬───────┘
       │
 ┌─────▼──────┐
 │ Drain CDC  │ → Read WAL changes into change buffer tables
 └─────┬──────┘
       │
 ┌─────▼──────────────┐
 │ Determine Action   │ → FULL, DIFFERENTIAL, NO_DATA, REINITIALIZE, or SKIP?
 │                    │   (adaptive: if change ratio > pg_stream.differential_max_change_ratio,
 │                    │    downgrade DIFFERENTIAL → FULL automatically)
 └─────┬──────────────┘
       │
 ┌─────▼──────┐
 │ Execute    │ → Full: TRUNCATE + INSERT ... SELECT
 │            │   Differential: Generate & apply delta SQL
 └─────┬──────┘
       │
 ┌─────▼──────────────┐
 │ Record History     │ → Write to pgstream.pgs_refresh_history
 └─────┬──────────────┘
       │
 ┌─────▼──────────────┐
 │ Advance Frontier   │ → Update JSONB frontier in catalog
 └─────┬──────────────┘
       │
 ┌─────▼──────────────┐
 │ Reset Error Count  │ → On success, reset consecutive_errors to 0
 └──────────────────────┘
```

### 8. Scheduling (`src/scheduler.rs`)

Automatic refresh scheduling uses **canonical periods** (48·2ⁿ seconds, n = 0, 1, 2, …) snapped to the user's `schedule`:

- Picks the smallest canonical period ≤ `schedule`.
- For **DOWNSTREAM** schedule (NULL schedule), the ST refreshes only when explicitly triggered or when a downstream ST needs it.
- Advisory locks prevent concurrent refreshes of the same ST.
- The scheduler is driven by a background worker polling at the `pg_stream.scheduler_interval_ms` GUC interval.

#### Shared Memory (`src/shmem.rs`)

The scheduler background worker and user sessions share a `PgStreamSharedState` structure protected by a `PgLwLock`. Key fields:

| Field | Type | Purpose |
|---|---|---|
| `dag_version` | `u64` | Incremented when the ST catalog changes; used by the scheduler to detect when the DAG needs rebuilding. |
| `scheduler_pid` | `i32` | PID of the scheduler background worker (0 if not running). |
| `scheduler_running` | `bool` | Whether the scheduler is active. |
| `last_scheduler_wake` | `i64` | Unix timestamp of the last scheduler wake cycle (for monitoring). |

A separate `PgAtomic<AtomicU64>` named `DAG_REBUILD_SIGNAL` is incremented by API functions (`create`, `alter`, `drop`) after catalog mutations. The scheduler compares its local copy against the atomic counter to detect when to rebuild its in-memory DAG without holding a lock.

### 9. DDL Tracking (`src/hooks.rs`)

Event triggers monitor DDL changes to source tables:

- **`_on_ddl_end`** — Fires on `ALTER TABLE` to detect column adds/drops/type changes. If a source table used by a ST is altered, the ST's `needs_reinit` flag is set.
- **`_on_sql_drop`** — Fires on `DROP TABLE` to set `needs_reinit` for affected STs.

Reinitialization is deferred until the next refresh cycle, which then performs a `REINITIALIZE` action (drop and recreate the storage table from the updated query).

### 10. Error Handling (`src/error.rs`)

Centralized error types using `thiserror`:

- `PgStreamError` variants cover catalog access, SQL execution, CDC, DVM, DAG, and config errors.
- Each refresh failure increments `consecutive_errors`.
- When `consecutive_errors` reaches `pg_stream.max_consecutive_errors` (default 3), the ST is moved to `ERROR` status and suspended from automatic refresh.
- Manual intervention (`ALTER ... status => 'ACTIVE'`) resets the counter.

### 11. Monitoring (`src/monitor.rs`)

Provides observability functions:

- **st_refresh_stats** — Aggregate statistics (total/successful/failed refreshes, avg duration, staleness status).
- **get_refresh_history** — Per-ST audit trail.
- **get_staleness** — Current staleness in seconds.
- **slot_health** — Checks replication slot state and WAL retention.
- **check_cdc_health** — Per-source CDC health status including mode, slot lag, confirmed LSN, and alerts.
- **explain_st** — Describes the DVM plan for a given ST.
- **Views** — `pgstream.stream_tables_info` (computed staleness) and `pgstream.pg_stat_stream_tables` (combined stats).

#### NOTIFY Alerting

Operational events are broadcast via PostgreSQL `NOTIFY` on the `pg_stream_alert` channel. Clients can subscribe with `LISTEN pg_stream_alert;` and receive JSON-formatted events:

| Event | Condition |
|---|---|
| `stale` | data staleness exceeds 2× `schedule` |
| `auto_suspended` | ST suspended after `pg_stream.max_consecutive_errors` failures |
| `reinitialize_needed` | Upstream DDL change detected |
| `slot_lag_warning` | Replication slot WAL retention is growing |
| `cdc_transition_complete` | Source transitioned from trigger to WAL-based CDC |
| `cdc_transition_failed` | Trigger→WAL transition failed (fell back to triggers) |
| `refresh_completed` | Refresh completed successfully |
| `refresh_failed` | Refresh failed with an error |

### 12. Row ID Hashing (`src/hash.rs`)

Provides deterministic 64-bit row identifiers using **xxHash (xxh64)** with a fixed seed. Two SQL functions are exposed:

- **`pgstream.pg_stream_hash(text)`** — Hash a single text value; used for simple single-column row IDs.
- **`pgstream.pg_stream_hash_multi(text[])`** — Hash multiple values (separated by a record-separator byte `\x1E`) for composite keys (join row IDs, GROUP BY keys).

Row IDs are written into every stream table's storage as an internal `__pgs_row_id BIGINT` column and are used by the delta application phase to match `DELETE` candidates precisely.

### 13. Configuration (`src/config.rs`)

Ten GUC (Grand Unified Configuration) variables control runtime behavior. See [CONFIGURATION.md](CONFIGURATION.md) for details.

| GUC | Default | Purpose |
|---|---|---|
| `pg_stream.enabled` | `true` | Master on/off switch for the scheduler |
| `pg_stream.scheduler_interval_ms` | `1000` | Scheduler background worker wake interval (ms) |
| `pg_stream.min_schedule_seconds` | `60` | Minimum allowed `schedule` |
| `pg_stream.max_consecutive_errors` | `3` | Errors before auto-suspending a ST |
| `pg_stream.change_buffer_schema` | `pgstream_changes` | Schema for change buffer tables |
| `pg_stream.max_concurrent_refreshes` | `4` | Maximum parallel refresh workers |
| `pg_stream.differential_max_change_ratio` | `0.15` | Change-to-table-size ratio above which DIFFERENTIAL falls back to FULL |
| `pg_stream.cleanup_use_truncate` | `true` | Use `TRUNCATE` instead of `DELETE` for change buffer cleanup when the entire buffer is consumed |
| `pg_stream.user_triggers` | `'auto'` | User-defined trigger handling: `auto` / `on` / `off` |
| `pg_stream.cdc_mode` | `'trigger'` | CDC mechanism: `trigger` / `auto` / `wal` |
| `pg_stream.wal_transition_timeout` | `300` | Max seconds to wait for WAL decoder catch-up during transition |

---

## Data Flow: End-to-End Refresh

```
 Source Table INSERT/UPDATE/DELETE
           │
           ▼
 Hybrid CDC Layer:
   ┌─────────────────────────────────────────────┐
   │ TRIGGER mode: Row-Level AFTER Trigger        │
   │   pg_stream_cdc_fn_<oid>() → buffer table    │
   │                                              │
   │ WAL mode: Logical Replication Slot           │
   │   wal_decoder bgworker → same buffer table   │
   └─────────────────────────────────────────────┘
           │
           ▼
 Change Buffer Table (pgstream_changes.changes_<oid>)
   Columns: change_id, lsn, xid, action (I/U/D), row_data (jsonb)
           │
           ▼
 DVM Engine: generate delta SQL from operator tree
   - Scan operator reads from change buffer
   - Filter/Project/Join transform the deltas
   - Aggregate recomputes affected groups
           │
           ▼
 Diff Engine: produce (+/-) diff rows
           │
           ▼
 Delta Application:
   DELETE FROM storage WHERE __pgs_row_id IN (removed)
   INSERT INTO storage SELECT ... FROM (added)
           │
           ▼
 Frontier Update: advance per-source LSN
           │
           ▼
 History Record: log to pgstream.pgs_refresh_history
```

---

## Module Map

```
src/
├── lib.rs           # Extension entry, module declarations, _PG_init
├── bin/
│   └── pgrx_embed.rs# pgrx SQL entity embedding (generated)
├── api.rs           # SQL API functions (create/alter/drop/refresh/status)
├── catalog.rs       # Catalog CRUD operations
├── cdc.rs           # Change data capture (triggers + WAL transition)
├── config.rs        # GUC variable registration
├── dag.rs           # Dependency graph (cycle detection, topo sort)
├── error.rs         # Centralized error types
├── hash.rs          # xxHash row ID generation (pg_stream_hash / pg_stream_hash_multi)
├── hooks.rs         # DDL event trigger handlers (_on_ddl_end, _on_sql_drop)
├── shmem.rs         # Shared memory state (PgStreamSharedState, DAG_REBUILD_SIGNAL)
├── dvm/
│   ├── mod.rs       # DVM module root + recursive CTE recomputation diff
│   ├── parser.rs    # Query → OpTree converter (CTE extraction, subquery, window support)
│   ├── diff.rs      # Delta SQL generation (CTE delta cache)
│   ├── row_id.rs    # Row ID generation
│   └── operators/
│       ├── mod.rs           # Operator trait + registry
│       ├── scan.rs          # Table scan (CDC passthrough)
│       ├── filter.rs        # WHERE clause filtering
│       ├── project.rs       # Column projection
│       ├── join.rs          # Inner join
│       ├── outer_join.rs    # LEFT/RIGHT/FULL outer join
│       ├── aggregate.rs     # GROUP BY + aggregate functions
│       ├── distinct.rs      # DISTINCT deduplication
│       ├── union_all.rs     # UNION ALL merging
│       ├── intersect.rs     # INTERSECT / INTERSECT ALL (dual-count LEAST)
│       ├── except.rs        # EXCEPT / EXCEPT ALL (dual-count GREATEST)
│       ├── subquery.rs      # Subquery / inlined CTE delegation
│       ├── cte_scan.rs      # Shared CTE delta (multi-reference)
│       ├── recursive_cte.rs # Recursive CTE (recomputation diff)
│       ├── window.rs        # Window function (partition recomputation)
│       └── lateral_function.rs # LATERAL SRF (row-scoped recomputation)
├── monitor.rs       # Monitoring & observability functions
├── refresh.rs       # Refresh orchestration
├── scheduler.rs     # Automatic scheduling with canonical periods
├── version.rs       # Frontier / LSN tracking
└── wal_decoder.rs   # WAL-based CDC (logical replication slot polling, transitions)
```

### Extension Control File (`pg_stream.control`)

The `pg_stream.control` file in the repository root is required by PostgreSQL's
extension infrastructure. It declares the extension's description, default
version, shared-library path, and privilege requirements. PostgreSQL reads this
file when `CREATE EXTENSION pg_stream;` is executed.

During packaging (`cargo pgrx package`), pgrx replaces the `@CARGO_VERSION@`
placeholder with the version from `Cargo.toml` and copies the file into the
target's `share/extension/` directory alongside the SQL migration scripts.
