# PLAN_EXTERNAL_PROCESS.md — External Sidecar Process Architecture

> **Status:** Exploration / Feasibility Study  
> **Target version:** Post-1.0 (if pursued)  
> **Author:** pg_trickle project

---

## 1. Motivation

pg_trickle currently ships as a **PostgreSQL C extension** (via pgrx), requiring:

- `shared_preload_libraries = 'pg_trickle'` (server restart)
- Write access to the `$PGDIR/lib/` and `$PGDIR/share/extension/` directories
- PostgreSQL 18.x (exact major version match)
- `CREATE EXTENSION` superuser privileges

This makes it **unusable on many managed PostgreSQL services** (AWS RDS, Azure
Flexible Server, Google Cloud SQL, Supabase, Neon, etc.) where users cannot
install custom C extensions or modify `shared_preload_libraries`.

Products like **Epsio** demonstrate that incremental view maintenance can be
delivered as an **external sidecar process** that connects to PostgreSQL over
a standard client connection (libpq/pgwire), removing all extension installation
requirements.

This document explores what it would take to ship pg_trickle as an external
process, what architectural changes are needed, and what trade-offs arise.

---

## 2. Current Architecture: PostgreSQL Coupling Inventory

Every major subsystem has dependencies on running inside PostgreSQL. Here is a
complete inventory:

### 2.1. Deep Coupling (Requires Fundamental Redesign)

| Component | PG Internal API Used | Why It's Coupled |
|-----------|---------------------|-----------------|
| **Background Worker** (`scheduler.rs`) | `BackgroundWorkerBuilder`, `BackgroundWorker::wait_latch`, `SIGHUP`/`SIGTERM` handlers, `BackgroundWorker::connect_worker_to_spi` | The scheduler *is* a PG bgworker. In sidecar mode, this becomes a standalone Rust async process. |
| **Shared Memory** (`shmem.rs`) | `PgLwLock`, `PgAtomic`, `pg_shmem_init!()` | DAG rebuild signal and cache generation counters use PG shared memory. Sidecar would need its own IPC or simply poll the catalog. |
| **Event Triggers / DDL Hooks** (`hooks.rs`) | `pg_event_trigger_ddl_commands()`, event trigger registration via `extension_sql!()` | DDL detection (ALTER/DROP on source tables) fires in-process. Sidecar would need to poll `pg_catalog` or use `LISTEN/NOTIFY`. |
| **SQL Parser** (`dvm/parser.rs`) | `pg_sys::raw_parser()`, node-tree walking (`T_SelectStmt`, `T_FuncCall`, etc.) | The DVM parser walks PG's raw parse tree in C structs. This is the **#1 hardest dependency** — sidecar needs an alternative parser. |
| **Volatility Analysis** (`dvm/parser.rs`) | `pg_sys::raw_parser()` + SPI to `pg_proc` | Walks parse tree nodes to classify function volatility. |
| **SPI (Server Programming Interface)** | `Spi::connect()`, `Spi::run()`, `Spi::get_one()` throughout `catalog.rs`, `cdc.rs`, `refresh.rs`, `monitor.rs`, `hooks.rs`, `wal_decoder.rs` | All catalog reads, change buffer reads, refresh execution, and DDL use in-process SPI. |

### 2.2. Moderate Coupling (Replaceable with Standard SQL)

| Component | PG Internal API Used | Sidecar Alternative |
|-----------|---------------------|-------------------|
| **Catalog CRUD** (`catalog.rs`) | SPI queries on `pgtrickle.*` tables | Standard SQL over libpq — straightforward port. |
| **CDC Triggers** (`cdc.rs`) | `CREATE TRIGGER` / `CREATE FUNCTION` via SPI | Create triggers via standard SQL connection — no change to trigger logic. |
| **Change Buffer Management** | SPI queries on `pgtrickle_changes.*` | Standard SQL queries — straightforward port. |
| **Refresh Execution** (`refresh.rs`) | SPI for `TRUNCATE`, `INSERT ... SELECT`, `DELETE`, `MERGE`, `SET LOCAL` | Execute via standard SQL connection in a transaction. |
| **Frontier / Version Tracking** (`version.rs`) | SPI to read/update JSONB frontiers | Standard SQL — straightforward. |
| **Hash Functions** (`hash.rs`) | `#[pg_extern]` exposing xxHash | Can ship as a small SQL-only extension, or use `md5()`/`hashtext()`, or compute hashes client-side and `INSERT` precomputed values. |
| **GUC Configuration** (`config.rs`) | `GucRegistry::define_*` | Replace with a config file (TOML/YAML) or environment variables. |
| **NOTIFY Alerting** (`monitor.rs`) | `Spi::run("NOTIFY pg_trickle_alert, ...")` | `NOTIFY` works from a standard client connection (`SELECT pg_notify(...)`). |

### 2.3. No Coupling (Pure Rust Logic)

| Component | Notes |
|-----------|-------|
| **DAG** (`dag.rs`) | Pure Rust graph algorithms — no PG dependency. |
| **Error Types** (`error.rs`) | Pure Rust `thiserror` enum. |
| **DVM Operators** (`dvm/operators/*.rs`) | Pure Rust SQL string generation — **no PG calls** in operators. |
| **DVM Diff** (`dvm/diff.rs`) | Pure SQL string generation — no SPI or pg_sys calls. |
| **DVM Row ID** (`dvm/row_id.rs`) | Pure Rust xxHash computation. |
| **Scheduling Logic** (`scheduler.rs` core logic) | The scheduling algorithm (canonical periods, topo ordering, retry/backoff) is pure logic wrapped in pgrx bgworker scaffolding. |

---

## 3. The Hard Problem: SQL Parsing

The single biggest obstacle is the DVM parser. Currently it:

1. Calls `pg_sys::raw_parser()` to parse the defining query into PG's internal
   `Node` tree (C structs)
2. Walks the tree recursively to build an `OpTree` (operator tree)
3. Uses the parse tree for CTE detection, subquery analysis, join
   classification, window function extraction, aggregate identification, etc.
4. Walks function calls in the tree to look up volatility in `pg_proc`

### 3.1. Alternative Parsing Strategies

| Strategy | Effort | Fidelity | Notes |
|----------|--------|----------|-------|
| **A. pg_query (libpg_query)** | **Medium** | **100%** | Uses the actual PG parser extracted into a standalone C library. `pg_query.rs` Rust bindings exist. Produces the same parse tree as `raw_parser()` but as protobuf messages — would need to rewrite tree-walking code against protobuf structs instead of `pg_sys::Node`. This is what most external PG tools use (pganalyze, Supabase, etc.). |
| **B. sqlparser-rs** | **Medium-High** | **~85-90%** | Pure Rust SQL parser with PostgreSQL dialect. Misses some PG-specific syntax (custom operators, PG type casts, some window frame edge cases). Would require manual handling of gaps. |
| **C. Remote parsing service** | **Low** | **100%** | Call a helper function installed on the target PG instance that parses the query and returns the parse tree as JSON. E.g., `pg_query_parse()` from the `pg_query` extension, or a custom function. Adds a network round-trip but gives 100% fidelity. |
| **D. Hybrid: generate SQL, let PG validate** | **Low-Medium** | **100%** | Don't parse internally — send the defining query to PG, use `EXPLAIN` or `pg_query_parse()` to extract plan/parse info, and build the operator tree from the response. |

**Recommendation: Strategy A (libpg_query/pg_query.rs)** is the best balance.
It maintains 100% parse fidelity, is widely proven (pganalyze processes
billions of queries with it), and avoids runtime network round-trips. The Rust
bindings (`pg_query.rs`) emit protobuf `ParseResult` structs that closely mirror
PG's internal `Node` types.

### 3.2. Parser Migration Scope

The parse-tree walking code lives primarily in:
- `src/dvm/parser.rs` — ~2,400 lines of node tree walking
- `src/hooks.rs` — DDL command parsing (but this goes away in sidecar mode)

Migrating from `pg_sys::Node` to `pg_query::protobuf::*` is a **mechanical but
large** refactor. The node types map 1:1 (e.g., `pg_sys::SelectStmt` →
`pg_query::protobuf::SelectStmt`), but field access patterns differ
(C pointer dereference vs. protobuf Option fields).

Estimated effort: **2-4 weeks** for a complete parser migration with tests.

---

## 4. Proposed Sidecar Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                   pg_trickle sidecar process                     │
│                                                                  │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────────┐    │
│  │  Config     │  │  Scheduler   │  │  Connection Pool     │    │
│  │  (TOML/env) │  │  (tokio)     │  │  (deadpool-postgres  │    │
│  └─────────────┘  └──────┬───────┘  │   or bb8-postgres)   │    │
│                          │          └──────────┬───────────┘    │
│                          │                     │                │
│  ┌───────────────────────▼─────────────────────▼──────────┐    │
│  │                   Refresh Engine                        │    │
│  │  ┌──────────┐  ┌──────────┐  ┌───────────────────────┐  │    │
│  │  │ Frontier │  │   DAG    │  │  DVM Engine            │  │    │
│  │  │ Tracker  │  │ Resolver │  │  (pg_query parser +    │  │    │
│  │  │          │  │          │  │   operator tree +      │  │    │
│  │  │          │  │          │  │   delta SQL gen)       │  │    │
│  │  └──────────┘  └──────────┘  └───────────────────────┘  │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌───────────────────────────────────────────────────────┐      │
│  │                  CDC Manager                          │      │
│  │  Install triggers via SQL │ Read change buffers       │      │
│  │  OR logical replication protocol (pgoutput)           │      │
│  └───────────────────────────────────────────────────────┘      │
│                                                                  │
│  ┌───────────────────────────────────────────────────────┐      │
│  │                  DDL Watcher                          │      │
│  │  LISTEN pg_trickle_ddl │ Poll pg_catalog fingerprints │      │
│  └───────────────────────────────────────────────────────┘      │
│                                                                  │
│  ┌───────────────────────────────────────────────────────┐      │
│  │              HTTP API / Metrics / Health               │      │
│  │  Prometheus endpoint │ REST management API             │      │
│  └───────────────────────────────────────────────────────┘      │
└───────────────────┬──────────────────────────────────────────────┘
                    │
                    │  Standard PostgreSQL wire protocol (libpq)
                    │
┌───────────────────▼──────────────────────────────────────────────┐
│                     PostgreSQL Instance                           │
│                                                                  │
│  ┌──────────────────────┐  ┌───────────────────────────────┐    │
│  │  Source Tables        │  │  Storage Tables (ST outputs)  │    │
│  │  (user data)          │  │  (created by sidecar via SQL) │    │
│  └──────────┬───────────┘  └───────────▲──────────────────┘    │
│             │                          │                        │
│  ┌──────────▼───────────┐  ┌───────────┴──────────────────┐    │
│  │  CDC Triggers         │  │  Catalog Tables              │    │
│  │  (installed by        │  │  (pgtrickle.pgt_* — created  │    │
│  │   sidecar via SQL)    │  │   by sidecar via SQL)        │    │
│  └──────────┬───────────┘  └──────────────────────────────┘    │
│             │                                                    │
│  ┌──────────▼───────────┐                                       │
│  │  Change Buffers       │                                       │
│  │  (pgtrickle_changes.*) │                                       │
│  └──────────────────────┘                                       │
└──────────────────────────────────────────────────────────────────┘
```

### 4.1. Component-by-Component Migration Plan

#### Scheduler → Tokio Async Runtime

Replace `BackgroundWorkerBuilder` + `wait_latch` with a `tokio` event loop:

```rust
#[tokio::main]
async fn main() {
    let config = load_config(); // TOML / env vars
    let pool = create_pg_pool(&config).await;
    
    let mut interval = tokio::time::interval(
        Duration::from_millis(config.scheduler_interval_ms)
    );
    
    loop {
        interval.tick().await;
        if let Err(e) = scheduler_tick(&pool, &mut state).await {
            tracing::error!("scheduler tick failed: {e}");
        }
    }
}
```

#### SPI → Connection Pool (tokio-postgres / deadpool-postgres)

All `Spi::connect()` / `Spi::run()` / `Spi::get_one()` calls become:

```rust
// Before (in-process SPI):
let count = Spi::get_one::<i64>("SELECT count(*) FROM pgtrickle.pgt_stream_tables")?;

// After (external client):
let row = pool.get().await?.query_one(
    "SELECT count(*) FROM pgtrickle.pgt_stream_tables", &[]
).await?;
let count: i64 = row.get(0);
```

This is a **mechanical refactor** — the SQL is identical, only the execution
mechanism changes from in-process SPI to client-side pgwire.

#### Shared Memory → Catalog Polling or LISTEN/NOTIFY

Replace `PgAtomic<DAG_REBUILD_SIGNAL>` with:

- **Option A (polling):** Read a generation counter from a catalog table
  (`pgtrickle.pgt_metadata`) on each scheduler tick.
- **Option B (LISTEN/NOTIFY):** The sidecar `LISTEN`s on a channel. API
  functions (which could be thin SQL wrappers or sidecar HTTP endpoints)
  `NOTIFY` on catalog changes. This is lower latency than polling.

Since the sidecar owns all writes, it can track its own generation counter
in-memory and only needs external signaling for concurrent API calls.

#### Event Triggers → DDL Detection

Three options, from simplest to most robust:

1. **Schema fingerprinting (poll-based):** On each scheduler tick, hash the
   column definitions of tracked source tables from `information_schema.columns`.
   If the fingerprint changes, mark the ST for reinit. Already partially
   implemented via `schema_fingerprint` in `pgt_dependencies`.

2. **LISTEN/NOTIFY from a tiny helper trigger:** Install a simple PL/pgSQL
   event trigger that does `PERFORM pg_notify('pg_trickle_ddl', ...)` on
   `ddl_command_end`. This requires `CREATE EVENT TRIGGER` privilege but does
   **not** require a C extension. The sidecar subscribes via `LISTEN`.

3. **Logical replication DDL messages (PG 16+):** DDL changes can be captured
   via logical replication if using `wal2json` or `pgoutput` with appropriate
   options. Limited and not universally available.

**Recommendation:** Start with (1) schema fingerprinting. It's zero-privilege
and works on all managed providers. Add (2) as an optimization when the target
PG allows event triggers.

#### CDC → Triggers-over-SQL or Logical Replication Protocol

**Trigger mode** works unchanged — the sidecar simply executes `CREATE TRIGGER`
and `CREATE FUNCTION` SQL statements over a standard connection. The trigger
functions are PL/pgSQL, not C, so they install without any extension.

**WAL mode** becomes even more natural: instead of a bgworker polling a
replication slot, the sidecar connects using the **streaming replication
protocol** (`START_REPLICATION SLOT ... LOGICAL ...`) directly. Libraries like
`tokio-postgres` support the replication protocol. This is how Debezium, Epsio,
and many CDC tools work.

> **Note:** WAL-based CDC requires `wal_level = logical` on the remote PG
> instance. Many managed providers support this (RDS, Cloud SQL, Azure Flex).

#### DVM Parser → pg_query.rs

Replace `pg_sys::raw_parser()` with `pg_query::parse()`:

```rust
// Before (in-process):
let parse_list = unsafe {
    pg_sys::raw_parser(c_query.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT)
};

// After (external, via pg_query.rs):
let result = pg_query::parse(query_str)?;
for stmt in &result.protobuf.stmts {
    // Walk protobuf SelectStmt, JoinExpr, etc.
}
```

The `pg_query` crate links against `libpg_query` (a standalone extraction of
PG's parser), so it runs entirely in the sidecar process with no PG connection
needed to parse SQL.

#### Hash Functions → Pure Rust or Minimal SQL

The `pgtrickle.pg_trickle_hash()` / `pg_trickle_hash_multi()` SQL functions are
used in delta queries. Two options:

1. **Inline the hash in generated SQL** using PG's built-in `hashtext()` or
   `md5()` — slightly different hash distribution but functional.
2. **Install a minimal SQL-only wrapper** that uses `hashtext()` under the
   hood — no C extension needed.
3. **Create the hash function as a PL/pgSQL function** installed by the
   sidecar at setup time.

**Recommendation:** Option 3 — install a PL/pgSQL `pgtrickle.pg_trickle_hash()`
that uses `hashtextextended(value, seed)` (PG 12+). No C extension needed.

#### Configuration → TOML Config File + Env Vars

```toml
# pg_trickle.toml
[connection]
host = "localhost"
port = 5432
database = "mydb"
user = "pgtrickle_user"
password_env = "PG_TRICKLE_PASSWORD"  # read from env var

[scheduler]
enabled = true
interval_ms = 1000

[cdc]
mode = "trigger"  # trigger | wal | auto

[refresh]
max_consecutive_errors = 3
differential_max_change_ratio = 0.15

[http]
listen = "0.0.0.0:9187"  # Prometheus metrics + REST API
```

#### Management API → HTTP + SQL Functions

In sidecar mode, users interact via:

1. **SQL functions** — The sidecar installs PL/pgSQL wrapper functions
   (`pgtrickle.create_stream_table(...)`) that write to the catalog tables.
   The sidecar picks up new entries on the next scheduler tick.
2. **HTTP API** — A REST API for management, monitoring, and triggering
   manual refreshes. Returns JSON.
3. **CLI** — A `pgtrickle` binary with subcommands (`create`, `drop`,
   `refresh`, `status`).

---

## 5. Deployment Modes

The sidecar approach enables multiple deployment topologies:

| Mode | Description | Target Audience |
|------|-------------|-----------------|
| **Docker sidecar** | Run alongside PG in a Docker Compose / K8s pod | Self-hosted, cloud VMs |
| **Kubernetes operator** | CRD-based management with auto-sidecar injection | K8s-native deployments |
| **Managed service agent** | Lightweight binary connecting to RDS/Cloud SQL | Managed PG users |
| **Lambda / Cloud Run** | Scheduled invocations (no persistent process) | Serverless / batch |
| **Embedded library** | Link `libpgtrickle` into an application process | Application embedding |

---

## 6. Feature Parity Matrix

| Feature | Extension Mode | Sidecar Mode | Notes |
|---------|---------------|--------------|-------|
| CREATE/ALTER/DROP stream table | ✅ | ✅ | SQL functions or HTTP API |
| Automatic scheduling | ✅ | ✅ | Tokio runtime vs. bgworker |
| Differential refresh | ✅ | ✅ | Same delta SQL, different execution path |
| Full refresh | ✅ | ✅ | Same SQL |
| CDC via triggers | ✅ | ✅ | PL/pgSQL triggers installed via SQL |
| CDC via WAL | ✅ | ✅ | Replication protocol — actually *easier* externally |
| DDL tracking (event triggers) | ✅ | ⚠️ Partial | Schema fingerprinting as fallback; event triggers where allowed |
| Shared memory signaling | ✅ | ❌ N/A | Replaced by LISTEN/NOTIFY or polling |
| Sub-millisecond refresh latency | ✅ | ❌ Slower | Network round-trip adds ~1-5ms per query |
| Zero-install on managed PG | ❌ | ✅ | **Key advantage** |
| Multi-database support | ❌ (1 DB) | ✅ | Single sidecar can manage multiple databases |
| Prometheus metrics | ❌ | ✅ | HTTP metrics endpoint |
| `shared_preload_libraries` required | ✅ | ❌ | **Key advantage** |
| Transaction-local visibility | ✅ | ❌ | Sidecar sees committed data only |

### Key Trade-offs

1. **Performance:** In-process SPI avoids network serialization/deserialization.
   For large differential refreshes (millions of delta rows), the sidecar may
   be **10-30% slower** due to pgwire overhead. For typical workloads (<100K
   deltas), the difference is negligible.

2. **Transaction atomicity:** The extension can participate in the user's
   transaction (trigger + buffer write are atomic). The sidecar operates on
   committed data — there's a small window where a trigger has written to the
   buffer but the source transaction hasn't committed. This is mitigated by
   the frontier/LSN mechanism that already handles this correctly.

3. **DDL detection fidelity:** Event triggers catch DDL changes immediately
   and in-transaction. Schema fingerprinting adds polling latency (up to one
   scheduler interval). For most use cases, this is acceptable.

---

## 7. Implementation Phases

### Phase S0: Crate Restructuring (2-3 weeks)

Split the monolithic crate into a workspace:

```
pg-trickle/
├── Cargo.toml                     # workspace root
├── crates/
│   ├── pgtrickle-core/            # Pure Rust: DAG, DVM operators, diff,
│   │                              # row_id, error types, scheduling logic
│   ├── pgtrickle-parser/          # SQL parsing via pg_query.rs
│   │                              # (replaces pg_sys::raw_parser)
│   ├── pgtrickle-client/          # PostgreSQL client layer
│   │                              # (tokio-postgres, connection pool,
│   │                              #  catalog CRUD, CDC management)
│   ├── pgtrickle-extension/       # pgrx extension wrapper
│   │                              # (thin shim: #[pg_extern] → core)
│   └── pgtrickle-sidecar/         # Sidecar binary
│                                  # (tokio runtime, HTTP API, config)
```

This refactor does NOT change any functionality — it separates pure logic from
PostgreSQL-specific code so both the extension and sidecar can share the core.

### Phase S1: pg_query.rs Parser Migration (2-4 weeks)

1. Add `pg_query` dependency to `pgtrickle-parser`
2. Rewrite `parse_defining_query()` to use protobuf AST nodes
3. Rewrite `walk_node_for_volatility()` to use protobuf FuncCall nodes
4. Rewrite `query_has_recursive_cte()` to use protobuf SelectStmt
5. Rewrite auto-rewrite passes to operate on protobuf AST
6. Rewrite all unit tests to run without a PG backend
7. Verify parse equivalence via integration tests (same query → same OpTree)

> **Risk:** The auto-rewrite passes (`rewrite_views_inline`, etc.) currently
> use string manipulation, not AST rewriting. These should continue to work
> as-is — they produce SQL strings that are then re-parsed.

### Phase S2: Client Layer (2-3 weeks)

1. Implement `PgClient` trait abstracting database access:
   ```rust
   #[async_trait]
   pub trait PgClient {
       async fn query(&self, sql: &str, params: &[&(dyn ToSql)]) -> Result<Vec<Row>>;
       async fn execute(&self, sql: &str, params: &[&(dyn ToSql)]) -> Result<u64>;
       async fn query_one(&self, sql: &str, params: &[&(dyn ToSql)]) -> Result<Row>;
       async fn transaction<F, T>(&self, f: F) -> Result<T>;
   }
   ```
2. Implement `SpiClient` (wrapping pgrx SPI) for the extension
3. Implement `TokioClient` (wrapping deadpool-postgres) for the sidecar
4. Port `catalog.rs`, `version.rs`, `monitor.rs` to use the trait

### Phase S3: Sidecar Binary (2-3 weeks)

1. Tokio-based scheduler main loop
2. Config file parsing (TOML)
3. Connection pool management
4. Bootstrap: create schemas + catalog tables on first connect
5. CDC trigger installation
6. Refresh execution via SQL client

### Phase S4: Management & Observability (1-2 weeks)

1. HTTP server (axum) for REST API + Prometheus metrics
2. CLI tool wrapping the HTTP API
3. PL/pgSQL wrapper functions for SQL-based management

### Phase S5: WAL-Based CDC (1-2 weeks)

1. Logical replication protocol client (tokio-postgres replication mode)
2. pgoutput decoder (or wal2json)
3. Write decoded changes to buffer tables

### Phase S6: Testing & Parity Validation (2-3 weeks)

1. Run the full E2E test suite against the sidecar
2. Performance benchmarks: extension vs. sidecar
3. Managed PG provider testing (RDS, Cloud SQL, Azure Flex)

**Total estimated effort: 12-18 weeks** for a production-quality sidecar.

---

## 8. Dual-Mode Shipping Strategy

The sidecar does NOT replace the extension — both modes ship:

| Distribution | Format | Use Case |
|-------------|--------|----------|
| `pg_trickle` extension | `.so` + `.control` + `.sql` | Self-hosted PG where extensions are allowed |
| `pgtrickle` binary | Static binary / Docker image | Managed PG, Kubernetes, cloud agents |
| `pgtrickle` Docker image | `ghcr.io/pg-trickle/pgtrickle` | Docker Compose, K8s sidecar |
| `libpgtrickle-core` crate | Rust library | Embedding in custom applications |

The extension mode remains the **recommended** path for self-hosted PostgreSQL
due to lower latency, stronger transactional guarantees, and simpler operations
(single process). The sidecar opens the market to managed PG users.

---

## 9. Minimum Viable Sidecar (MVS)

For a quick proof-of-concept, a minimal sidecar could be built in **4-6 weeks**
by taking shortcuts:

1. **Skip parser migration** — Use Strategy C (remote parsing): install
   `pg_query` extension on the target PG and call a helper function. Falls
   back gracefully if the extension isn't installed.
2. **Trigger-only CDC** — No WAL mode initially.
3. **CLI-only management** — No HTTP API.
4. **Poll-only DDL detection** — Schema fingerprinting.
5. **Single-database** — No multi-DB support.

This MVS validates the concept and market fit before investing in the full
migration.

---

## 10. Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| pg_query.rs protobuf API doesn't cover all PG18 syntax | Parser gaps for edge cases | pg_query tracks PG releases closely; PG18 support expected shortly after GA |
| Performance regression for large refreshes | Slower refresh cycles | Batch delta SQL into larger `COPY`-based operations; prepared statements over pgwire |
| Managed PG providers block `CREATE FUNCTION` (PL/pgSQL) | Cannot install CDC triggers | Fall back to WAL-based CDC (requires `wal_level = logical`, which most providers support) |
| Connection pool exhaustion under many STs | Stalled refreshes | Configurable pool size; backpressure; sequential processing (same as extension mode) |
| Two codepaths to maintain | Maintenance overhead | Shared core crate; same SQL generation; abstracted client layer; shared test suite |
| Trigger-based CDC requires `superuser` or table ownership | Permission errors on managed PG | Document required privileges; provide RDS IAM policy templates; fall back to WAL CDC |

---

## 11. Competitive Landscape

| Product | Architecture | PG Versions | Managed PG Support |
|---------|-------------|-------------|-------------------|
| **Epsio** | External process (closed source) | 12-16 | Yes (RDS, Cloud SQL) |
| **pg_ivm** | Extension (C) | 14-17 | No (requires extension) |
| **pg_trickle (current)** | Extension (Rust/pgrx) | 18 | No (requires extension) |
| **pg_trickle (proposed sidecar)** | External process + optional extension | 14-18+ | Yes |
| **Materialize** | Separate database engine | N/A | Yes (own cloud) |
| **dbt** | External batch tool | All | Yes (no real-time) |

The sidecar mode would make pg_trickle the **first open-source incremental view
maintenance tool** that works on managed PostgreSQL instances.

---

## 12. Cross-Plan Impact Analysis

Two other proposed plans have significant interactions with the sidecar
architecture. This section analyses how they affect feasibility, design
decisions, and implementation sequencing.

### 12.1 Impact of PLAN_TRANSACTIONAL_IVM (Immediate Mode)

**Source:** [plans/sql/PLAN_TRANSACTIONAL_IVM.md](../sql/PLAN_TRANSACTIONAL_IVM.md)

The Transactional IVM plan proposes an `IMMEDIATE` refresh mode where stream
tables are updated **in the same transaction** as the base table DML, using
statement-level AFTER triggers with transition tables and in-process SPI
execution.

#### 12.1.1 IMMEDIATE Mode Is Fundamentally Incompatible with Sidecar

The core mechanism of IMMEDIATE mode requires:

1. **Statement-level AFTER triggers with transition tables** (`REFERENCING
   NEW TABLE AS ... OLD TABLE AS ...`) — these are PL/pgSQL or C trigger
   functions that fire **inside the user's transaction**.
2. **In-process delta computation** — the DVM engine generates delta SQL and
   executes it via SPI within the same transaction, before the triggering
   statement returns control to the user.
3. **Ephemeral Named Relations (ENRs)** — transition table data is accessed
   via ENRs registered in the query environment, which are only available
   from within the same backend process.
4. **ExclusiveLock on the stream table** — acquired from within the trigger,
   serializing concurrent DML.
5. **Snapshot management** — `CommandCounterIncrement()` and snapshot push/pop
   to make just-inserted rows visible within the trigger.

None of these can be performed from an external process. The sidecar connects
over pgwire and sees only **committed** data. It cannot participate in
another session's transaction, access its transition tables, or call
`CommandCounterIncrement()`.

**Verdict: IMMEDIATE mode is extension-only.** It cannot be offered in sidecar
mode. This is a fundamental architectural constraint, not a missing feature.

#### 12.1.2 Implications for Dual-Mode Strategy

This creates a clear **feature differentiation** between the two modes:

| Feature | Extension Mode | Sidecar Mode |
|---------|---------------|--------------|
| FULL refresh | ✅ | ✅ |
| DIFFERENTIAL refresh | ✅ | ✅ |
| IMMEDIATE refresh | ✅ | ❌ **Not possible** |
| pg_ivm compatibility layer | ✅ | ❌ **Not possible** |
| Read-your-writes consistency | ✅ (IMMEDIATE) | ❌ (eventual only) |

This means:
- The extension remains the only option for users who need **transactional
  consistency** (pg_ivm replacement use case).
- The sidecar serves the **analytics / dashboard / eventual-consistency**
  use case where sub-second staleness is acceptable.
- Marketing and documentation must clearly communicate this distinction.

#### 12.1.3 Shared DVM Code — But Different Delta Sources

The TRANSACTIONAL_IVM plan proposes a `DeltaSource` enum:

```rust
enum DeltaSource {
    ChangeBuffer { lsn_range: (Lsn, Lsn) },
    TransitionTable { old_tuplestore: ..., new_tuplestore: ... },
}
```

The sidecar would add a third variant:

```rust
enum DeltaSource {
    ChangeBuffer { lsn_range: (Lsn, Lsn) },      // Deferred (both modes)
    TransitionTable { old_enr: ..., new_enr: ... }, // Immediate (extension only)
    // No sidecar-specific variant needed — sidecar uses ChangeBuffer
}
```

The DVM operator tree and diff engine are **fully shared** across all three
modes. Only the `Scan` operator's delta SQL generation differs based on the
`DeltaSource`. This reinforces the workspace restructuring in Phase S0 —
the `pgtrickle-core` crate handles all modes, while `pgtrickle-extension`
adds the ENR/trigger machinery and `pgtrickle-sidecar` adds the pgwire
client.

#### 12.1.4 Sequencing Recommendation

The crate restructuring (Phase S0) should account for the `DeltaSource`
abstraction from day one, even if IMMEDIATE mode is implemented later. This
avoids a second restructuring when Transactional IVM lands.

Suggested order:
1. Phase S0: Restructure into workspace with `DeltaSource` enum in core
2. Build sidecar (Phases S1-S6) using `ChangeBuffer` variant
3. Implement IMMEDIATE mode in `pgtrickle-extension` using `TransitionTable`
   variant
4. Both can proceed in parallel after Phase S0

### 12.2 Impact of PLAN_DIAMOND_DEPENDENCY_CONSISTENCY

**Source:** [plans/PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md](../PLAN_DIAMOND_DEPENDENCY_CONSISTENCY.md)

The Diamond Consistency plan addresses the problem where a fan-in node D
(depending on B and C, which both depend on A) can see inconsistent versions
of A's data if B and C refresh at different times or one fails.

#### 12.2.1 Consistency Groups Work Differently in Sidecar Mode

The recommended solution (Option 1: Epoch-Based Atomic Refresh Groups) uses
PostgreSQL SAVEPOINTs to atomically commit or rollback a group of related
stream table refreshes:

```sql
SAVEPOINT consistency_group;
-- refresh B
-- refresh C
-- if both succeed: RELEASE SAVEPOINT
-- if any fails:     ROLLBACK TO SAVEPOINT
-- then refresh D (or skip)
```

**This works in sidecar mode**, but with important differences:

| Aspect | Extension Mode | Sidecar Mode |
|--------|---------------|--------------|
| **Transaction scope** | Single SPI transaction within bgworker | Single pgwire transaction from connection pool |
| **SAVEPOINT support** | Via `Spi::run("SAVEPOINT ...")` | Via `client.execute("SAVEPOINT ...", &[])` — identical SQL |
| **Lock holding** | In-process locks released on rollback | Client-held locks released on rollback — identical semantics |
| **Failure detection** | SPI error returned to Rust | pgwire error returned to Rust — identical |
| **Performance** | No network overhead | Network RTT per SAVEPOINT/RELEASE (~1-2ms) |

The atomic refresh group logic is **pure scheduling/orchestration** — it
lives in the scheduler main loop and uses standard SQL transaction control.
It ports cleanly to the sidecar.

#### 12.2.2 Frontier Alignment Is Actually Easier in Sidecar Mode

Option 2 (Frontier Alignment check: skip D if B and C have divergent
frontiers) requires comparing per-ST frontier LSNs before deciding whether
to refresh a fan-in node. In extension mode, this reads from the catalog
via SPI. In sidecar mode, it's the same catalog query over pgwire.

The sidecar may even have an advantage: since it maintains the DAG and
consistency groups **in its own memory** (not in PG shared memory), it can
cache the group detection results and frontier checks without the overhead
of shared memory synchronization.

#### 12.2.3 Version-Stamped Refresh (Option 4) Interacts with Storage Schema

Option 4 proposes adding a `__pgt_source_versions JSONB` column to every
stream table row to track which source versions contributed to each row.
If this approach were adopted (it is NOT the recommended option), it would
affect the sidecar because:

- The sidecar's bootstrap phase would need to add this column when creating
  storage tables.
- Delta SQL generation would need to propagate version metadata.
- The additional JSONB column increases storage and query overhead — this
  is amplified in sidecar mode where data transits over the network.

Since Option 4 is not recommended, this is a low-risk concern. But if it
were pursued, the sidecar should be considered during schema design.

#### 12.2.4 Diamond Consistency Configuration in Sidecar Mode

The proposed `pg_trickle.diamond_consistency` GUC would become a sidecar
config option instead:

```toml
# pg_trickle.toml (sidecar)
[scheduler]
diamond_consistency = "atomic"  # "atomic" | "aligned" | "none"
```

Per-ST overrides would be stored in the `pgt_stream_tables` catalog table
(a new `diamond_consistency` column), which both modes read. The sidecar
reads this via a standard SQL query rather than a GUC.

#### 12.2.5 Interaction Between Diamonds and IMMEDIATE Mode

The Diamond plan notes (§8.2) that IMMEDIATE mode **inherently avoids** the
diamond inconsistency problem because changes propagate within a single
transaction via trigger nesting. This is correct — and it reinforces the
feature matrix:

| Scenario | Extension | Sidecar |
|----------|-----------|---------|
| Diamond + DEFERRED | Needs consistency groups | Needs consistency groups |
| Diamond + IMMEDIATE | No problem (same-transaction) | N/A (IMMEDIATE not available) |

Since the sidecar only supports DEFERRED mode, diamond consistency is
**always relevant** for sidecar deployments with fan-in DAGs. The sidecar
should implement at least the frontier alignment check (Option 2) from the
start, with atomic groups as a follow-up.

#### 12.2.6 Sequencing Recommendation

Diamond detection (`detect_consistency_groups()` in `dag.rs`) is pure Rust
graph logic — no PG dependency. It should be implemented in `pgtrickle-core`
and shared by both modes.

Suggested order:
1. Phase 1 of Diamond plan (detection in `dag.rs`) — implement in core
2. Phase 2 (frontier alignment) — implement in scheduler abstraction layer
3. Sidecar gets diamond support "for free" via shared core
4. Phase 3 (atomic groups with SAVEPOINTs) — implement in both scheduler
   implementations (bgworker + tokio)

### 12.3 Combined Impact Summary

```
                    ┌─────────────────────────────────────────┐
                    │          Feature Availability            │
                    ├──────────────────┬──────────────────────┤
                    │  Extension Mode  │   Sidecar Mode       │
┌───────────────────┼──────────────────┼──────────────────────┤
│ DEFERRED refresh  │       ✅         │        ✅            │
│ FULL refresh      │       ✅         │        ✅            │
│ IMMEDIATE refresh │       ✅         │        ❌            │
│ pg_ivm compat     │       ✅         │        ❌            │
│ Diamond: atomic   │       ✅         │        ✅            │
│ Diamond: aligned  │       ✅         │        ✅            │
│ Diamond: none     │       ✅         │        ✅            │
│ CDC: triggers     │       ✅         │        ✅            │
│ CDC: WAL          │       ✅         │        ✅            │
│ DDL event triggers│       ✅         │     ⚠️ Partial       │
│ Managed PG        │       ❌         │        ✅            │
└───────────────────┴──────────────────┴──────────────────────┘
```

The key takeaway: **IMMEDIATE mode is the one feature that absolutely
requires the extension.** Everything else — including diamond consistency
— ports cleanly to the sidecar. This means the dual-mode strategy has a
clear value proposition for each mode:

- **Extension:** Full feature set including IMMEDIATE mode and pg_ivm
  compatibility. Best for self-hosted PG where maximum consistency matters.
- **Sidecar:** DEFERRED mode with diamond consistency. Best for managed PG
  where installation constraints prevent extension loading. Accepts eventual
  consistency in exchange for zero-install deployment.

### 12.4 Impact on Implementation Phases

The cross-plan considerations suggest the following adjustments to the sidecar
implementation timeline:

| Phase | Original Estimate | Adjusted | Reason |
|-------|------------------|----------|--------|
| S0: Crate restructuring | 2-3 weeks | **3-4 weeks** | Must also design `DeltaSource` abstraction and diamond detection API in core |
| S1: pg_query parser | 2-4 weeks | 2-4 weeks | No change — parser is mode-independent |
| S2: Client layer | 2-3 weeks | **3-4 weeks** | Must include transaction/SAVEPOINT abstraction for diamond atomic groups |
| S3: Sidecar binary | 2-3 weeks | 2-3 weeks | No change |
| S4: Management | 1-2 weeks | 1-2 weeks | No change |
| S5: WAL CDC | 1-2 weeks | 1-2 weeks | No change |
| S6: Testing | 2-3 weeks | **3-4 weeks** | Must test diamond consistency in sidecar mode; IMMEDIATE mode exclusion tests |
| **Total** | **12-18 weeks** | **15-22 weeks** | ~3-4 weeks added for cross-plan concerns |

---

## 13. Open Questions

1. **Minimum PG version for sidecar mode?** pg_query.rs supports PG 13+
   syntax parsing. The sidecar could potentially support PG 14-18+ while the
   extension remains PG18-only.

2. **Should the SQL API change?** The extension uses `#[pg_extern]` functions.
   The sidecar could install PL/pgSQL stubs that write to catalog tables, or
   expose a completely different HTTP-based API.

3. **Licensing implications?** libpg_query (used by pg_query.rs) is BSD-licensed,
   same as PostgreSQL itself. No licensing conflict.

4. **Should we support `pgwire` as a proxy?** The sidecar could intercept SQL
   traffic and transparently add CDC triggers — no user action needed. This is
   how Epsio works. Adds significant complexity.

5. **Should the sidecar clearly document IMMEDIATE mode as extension-only at
   setup time?** If a user tries to create an IMMEDIATE stream table via the
   sidecar, it should fail with a clear error message pointing them to the
   extension.

6. **Should diamond consistency default to `'aligned'` in sidecar mode?**
   Since sidecar users can't fall back to IMMEDIATE mode for same-transaction
   consistency, a stricter default for diamond handling may be warranted.

---

## 14. Verdict

**Yes, it is feasible** to ship pg_trickle as an external sidecar process. The
largest technical hurdle is the SQL parser migration (~2-4 weeks), but
`pg_query.rs` provides a proven, high-fidelity alternative. Most other
components (catalog, CDC, refresh, DAG, scheduling) migrate mechanically from
SPI to pgwire client calls.

The recommended approach is:

1. **Short-term:** Restructure the crate into a workspace (Phase S0) to
   cleanly separate pure Rust logic from pgrx-specific code. This benefits
   the extension build regardless.
2. **Medium-term:** Build a Minimum Viable Sidecar (4-6 weeks) to validate
   the concept with early adopters on managed PostgreSQL.
3. **Long-term:** Invest in the full sidecar with WAL CDC, HTTP API, and
   multi-database support once market fit is validated.
