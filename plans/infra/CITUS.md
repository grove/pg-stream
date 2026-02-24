# PLAN_CITUS.md — Citus Compatibility for pg_stream

## 1. Executive Summary

This plan makes pg_stream work transparently with Citus-distributed source tables and optionally distributed ST storage tables, with auto-detection at runtime. The extension currently has **zero multi-node awareness** — every core module assumes a single PostgreSQL instance with local OIDs, local WAL, local triggers, and a single background worker.

The plan touches all 13 source files across 7 phases, replacing OID-based naming with stable identifiers, LSN-based frontiers with a coordinator-managed logical sequence, local-only triggers with worker-propagated triggers, and single-node MERGE with Citus-compatible apply logic.

**Estimated scope:** ~6 months of focused engineering.

**Target:** Citus 13.x on PostgreSQL 18 (the extension already targets PG 18 exclusively).

---

## 2. Current Incompatibilities

### 2.1. OID-Based Change Buffer Naming (HIGH — pervasive)

Change buffer tables are named `pg_stream_changes.changes_<oid>` where `<oid>` is the **local PostgreSQL OID** of the source table. OIDs are **not globally unique** across Citus nodes. A table distributed across worker nodes will have different OIDs on each worker. The coordinator OID ≠ worker OIDs.

**~25 locations** across `src/cdc.rs`, `src/refresh.rs`, `src/dvm/operators/scan.rs`, `src/dvm/operators/aggregate.rs`, `src/dvm/operators/recursive_cte.rs`.

Affected patterns:
- Table creation: `CREATE TABLE IF NOT EXISTS {schema}.changes_{oid}`
- Trigger creation: `INSERT INTO {change_schema}.changes_{oid}`
- Trigger naming: `pg_stream_cdc_{oid}`
- Function naming: `pg_stream_cdc_fn_{oid}`
- Index creation: `idx_changes_{oid}_lsn_pk_cid`
- Cleanup: `DELETE FROM {schema}.changes_{oid}`
- Delta CTE SQL: `"{}.changes_{}"` keyed by `table_oid`
- Frontier storage: JSONB keyed by `"oid_string"` in `pg_stream.pgs_stream_tables.frontier`

### 2.2. `pg_current_wal_lsn()` as Change Frontier (HIGH — architectural)

The entire CDC and frontier system depends on WAL LSN as an ordering/versioning mechanism. In Citus, each worker has its own WAL with independent LSN sequences. `pg_current_wal_lsn()` on worker A returns a value incomparable to worker B.

Affected code:
- Trigger function: `VALUES (pg_current_wal_lsn(), 'I', ...)` — 7 call sites in `src/cdc.rs`
- `get_current_wal_lsn()` in `src/cdc.rs` (L380-L387)
- `SourceVersion.lsn` in `src/version.rs` (L37-L43)
- `Frontier.sources` HashMap keyed by OID string in `src/version.rs` (L29-L34)
- Change detection query: `WHERE lsn > ... AND lsn <=` in `src/refresh.rs` (L375-L393)
- LSN placeholder generation: `__PGDT_PREV_LSN_{oid}__` in `src/dvm/diff.rs` (L122-L137)
- LSN resolution: `resolve_lsn_placeholders()` in `src/refresh.rs` (L59-L72)

### 2.3. Trigger-based CDC on Distributed Tables (HIGH — fundamental)

Triggers are created on the coordinator but DML on distributed tables executes on workers. Citus does **not propagate** arbitrary triggers to workers. DML goes to workers directly, bypassing coordinator triggers entirely. Only Citus reference tables fire triggers on the coordinator.

Affected code:
- `CREATE TRIGGER ... ON {source_table}` in `src/cdc.rs` (L135-L145)
- `trigger_exists()` check in `src/cdc.rs` (L570-L576) — queries `pg_trigger` on local node
- `setup_cdc_for_source()` in `src/api.rs` (L519-L550)

### 2.4. MERGE Statement Compatibility (MEDIUM — query generation)

Citus has limited MERGE support. The USING subquery references local change buffer tables while the target may be distributed. JOIN between a distributed ST table and a local change buffer is a cross-shard join.

Affected code:
- MERGE generation in `src/refresh.rs` (L165-L175) — 2 MERGE templates
- `CachedMergeTemplate` in `src/refresh.rs` (L28-L52)

### 2.5. Shared Memory & Background Worker (MEDIUM — coordination)

1 background worker, 2 shared memory structures. All coordinator-local. Advisory locks are node-local.

Affected code:
- `BackgroundWorkerBuilder::new("pg_stream scheduler")` in `src/scheduler.rs` (L41-L48)
- `PGS_STATE: PgLwLock<PgStreamSharedState>` in `src/shmem.rs` (L31)
- `DAG_REBUILD_SIGNAL: PgAtomic<AtomicU64>` in `src/shmem.rs` (L37)
- `pg_try_advisory_lock(st.pgs_id)` in `src/scheduler.rs` (L281-L296, L425-L431)

### 2.6. System Catalog & Row Estimates (LOW — manageable)

`pg_class.reltuples` on the coordinator only reflects the coordinator's local shard (which is empty for distributed tables). Event triggers fire only on the coordinator.

Affected code:
- `pg_class WHERE oid = {oid}::oid` in `src/refresh.rs` (L371-L393)
- `pg_event_trigger_ddl_commands()` in `src/hooks.rs` (L55)

---

## 3. Design Decisions

| Concern | Decision | Rationale |
|---------|----------|-----------|
| **Naming** | Deterministic schema-name hash | Survives pg_dump/restore, predictable across nodes without coordination |
| **Frontier** | Citus-distributed sequence | Single total ordering without multi-frontier merge logic |
| **Change buffer placement** | Citus-distributed table | Best balance of write performance (local writes) and read simplicity (coordinator queries route to all workers) |
| **MERGE replacement** | INSERT ON CONFLICT + DELETE | Fully supported by Citus for distributed tables; MERGE support is limited |
| **Scheduler location** | Coordinator-only | Citus naturally routes distributed queries from coordinator; avoids multi-scheduler coordination |
| **Lock mechanism** | Catalog-based locks | Advisory locks are node-local; catalog locks are visible cluster-wide |
| **DAG signaling** | LISTEN/NOTIFY | Works across connections, naturally HA-safe; replaces shared memory atomics |
| **Detection** | Auto-detect Citus at runtime | Users don't need to set a GUC; extension adapts behavior automatically |
| **Backward compat** | OID columns kept alongside stable_name | Non-Citus deployments continue with zero behavior change; Citus path is additive |

---

## 4. Implementation Phases

### Phase 1: Citus Detection & Abstraction Layer (~2 weeks)

**Goal:** Add runtime Citus detection and introduce table placement abstractions used throughout subsequent phases.

**P1.1: Create `src/citus.rs` module**

Add a new module with detection utilities:

```rust
/// Check if Citus extension is loaded
pub fn is_citus_available() -> bool
// Queries: SELECT 1 FROM pg_extension WHERE extname = 'citus'

/// Check if a table is Citus-distributed
pub fn is_distributed_table(oid: pg_sys::Oid) -> bool
// Queries: SELECT 1 FROM pg_dist_partition WHERE logicalrelid = $1

/// Check if a table is a Citus reference table
pub fn is_reference_table(oid: pg_sys::Oid) -> bool
// Queries: SELECT partmethod = 'n' FROM pg_dist_partition WHERE logicalrelid = $1

/// Get the distribution column for a distributed table
pub fn get_distribution_column(oid: pg_sys::Oid) -> Option<String>
// Queries: SELECT column_to_column_name(logicalrelid, partkey)
//          FROM pg_dist_partition WHERE logicalrelid = $1

/// Get all worker nodes in the cluster
pub fn get_worker_nodes() -> Vec<(String, i32)>
// Queries: SELECT nodename, nodeport FROM pg_dist_node WHERE isactive AND noderole = 'primary'

/// Execute SQL on all nodes (coordinator + workers)
pub fn run_on_all_nodes(sql: &str) -> Result<(), PgStreamError>
// Wraps: SELECT run_command_on_all_nodes($1)

/// Execute SQL on workers only
pub fn run_on_workers(sql: &str) -> Result<(), PgStreamError>
// Wraps: SELECT run_command_on_workers($1)
```

**P1.2: Introduce `TablePlacement` enum**

```rust
pub enum TablePlacement {
    Local,
    CitusReference,
    CitusDistributed { dist_column: String },
}
```

Used throughout the codebase to branch behavior.

**P1.3: Enrich source dependencies with placement**

- Add `source_placement TEXT` column to `pg_stream.pgs_dependencies` in `src/lib.rs`
- In `extract_source_relations()` at `src/api.rs` (L702), after resolving OIDs, call `citus::is_distributed_table()` / `citus::is_reference_table()` to determine each source's placement
- Store placement in `StDependency` struct in `src/catalog.rs`

**Files modified:** `src/citus.rs` (new), `src/lib.rs`, `src/api.rs`, `src/catalog.rs`

---

### Phase 2: Stable Naming — Replace OID-Based Identifiers (~2 weeks)

**Goal:** Replace all OID-based object naming with a deterministic stable hash that is identical across Citus nodes.

**P2.1: Introduce `SourceIdentifier` type**

```rust
pub struct SourceIdentifier {
    pub oid: pg_sys::Oid,
    pub stable_name: String,  // pg_stream_hash(schema_name || '.' || table_name)
}
```

The `stable_name` is a deterministic short hash of the fully-qualified table name. This survives pg_dump/restore and is identical across all Citus nodes.

**P2.2: Rename change buffer objects**

Replace `changes_{oid}` with `changes_{stable_hash}` in all locations:

| Location | Current | New |
|----------|---------|-----|
| `src/cdc.rs` `create_change_buffer_table()` (L209) | `changes_{oid}` | `changes_{stable_hash}` |
| `src/cdc.rs` `create_change_trigger()` (L37) | `pg_stream_cdc_fn_{oid}` | `pg_stream_cdc_fn_{stable_hash}` |
| `src/cdc.rs` trigger name (L577) | `pg_stream_cdc_{oid}` | `pg_stream_cdc_{stable_hash}` |
| `src/cdc.rs` index name (L245-L259) | `idx_changes_{oid}_*` | `idx_changes_{stable_hash}_*` |
| `src/cdc.rs` `delete_consumed_changes()` (L430) | `changes_{oid}` | `changes_{stable_hash}` |
| `src/refresh.rs` cleanup (L617-L624) | `changes_{oid}` | `changes_{stable_hash}` |
| `src/dvm/operators/scan.rs` (L52) | `changes_{oid}` | `changes_{stable_hash}` |
| `src/dvm/operators/aggregate.rs` (L116) | `changes_{oid}` | `changes_{stable_hash}` |
| `src/dvm/operators/recursive_cte.rs` (L117) | `changes_{oid}` | `changes_{stable_hash}` |

**P2.3: Update frontier key format**

- `Frontier.sources` HashMap in `src/version.rs` (L29): Key changes from OID string to `stable_name`
- `get_lsn(source_oid)` → `get_lsn(stable_name)` and all callers
- `set_source(source_oid, ...)` → `set_source(stable_name, ...)`
- JSONB serialization automatically adapts (no schema change needed)

**P2.4: Update LSN placeholder tokens**

- `DiffContext.get_prev_lsn()` / `get_new_lsn()` in `src/dvm/diff.rs` (L122-L137): `__PGDT_PREV_LSN_{stable_name}__`
- `resolve_lsn_placeholders()` in `src/refresh.rs` (L59-L72): Match new placeholder format

**P2.5: Catalog migration**

- Add `source_stable_name TEXT` column to `pg_stream.pgs_dependencies` and `pg_stream.pgs_change_tracking` in `src/lib.rs`
- OID columns kept for local lookups; `stable_name` becomes the join key for cross-node operations
- Write SQL migration (`ALTER EXTENSION ... UPDATE`) that backfills `stable_name` for existing tracked sources by joining `pg_class` + `pg_namespace`

**Files modified:** `src/cdc.rs`, `src/version.rs`, `src/dvm/diff.rs`, `src/refresh.rs`, `src/dvm/operators/scan.rs`, `src/dvm/operators/aggregate.rs`, `src/dvm/operators/recursive_cte.rs`, `src/lib.rs`, `src/catalog.rs`, `src/api.rs`

---

### Phase 3: Logical Sequence Frontier — Replace WAL LSN (~3 weeks)

**Goal:** Replace `pg_current_wal_lsn()` with a coordinator-managed monotonic sequence that provides a single total ordering across all nodes.

**P3.1: Create coordinator sequence**

Add to catalog DDL in `src/lib.rs`:

```sql
CREATE SEQUENCE pg_stream.change_seq;
```

This provides globally-ordered, monotonically-increasing values that replace `pg_current_wal_lsn()`.

**P3.2: Introduce `FrontierMode` enum**

```rust
pub enum FrontierMode {
    Lsn,         // Single-node: use pg_current_wal_lsn()
    LogicalSeq,  // Citus: use nextval('pg_stream.change_seq')
}
```

Determined at ST creation time based on whether any source is `CitusDistributed`.

**P3.3: Dual-mode trigger function**

Modify the trigger function template in `src/cdc.rs` (L102-L131):

- **Local/reference sources:** Keep `pg_current_wal_lsn()` for backward compatibility
- **Distributed sources:** Use `nextval('pg_stream.change_seq')` instead:
  ```sql
  VALUES (nextval('pg_stream.change_seq'), 'I', ...);
  ```

**P3.4: Dual-mode change buffer schema**

For distributed sources, the `lsn PG_LSN` column in change buffer tables (`src/cdc.rs` L212) becomes `seq_id BIGINT`. The covering index changes from `(lsn, pk_hash, change_id)` to `(seq_id, pk_hash, change_id)`.

**P3.5: Extend `SourceVersion` with versioning modes**

In `src/version.rs` (L37-L43):

```rust
pub enum VersionMarker {
    Lsn(String),   // e.g. "0/1A2B3C4"
    Seq(i64),      // e.g. 42000
}
```

Update all frontier comparison logic. The `lsn_gt()` comparator at L109 needs a `seq_gt()` counterpart (simple integer comparison).

**P3.6: Update frontier acquisition**

`get_current_wal_lsn()` in `src/cdc.rs` (L380) becomes `get_current_frontier_position()`:
- Local mode: Returns `VersionMarker::Lsn(pg_current_wal_lsn())`
- Citus mode: Returns `VersionMarker::Seq(currval('pg_stream.change_seq'))`

**P3.7: Update change detection query**

In `src/refresh.rs` (L375-L393), the `WHERE lsn > ... AND lsn <=` clause becomes `WHERE seq_id > ... AND seq_id <=` for sequence-mode sources.

**P3.8: Distributed sequence propagation**

Citus sequences on the coordinator are not automatically available on workers. Options:
1. Use `citus.enable_ddl_propagation` to propagate `CREATE SEQUENCE`
2. Use `run_command_on_workers()` to create matching sequence on each worker
3. Use Citus metadata sequences with 2PC for global uniqueness

**Decision needed:** Benchmark which Citus sequence propagation mechanism works most reliably under high-frequency trigger calls. The trigger on each worker must call `nextval('pg_stream.change_seq')` and get globally-unique, monotonically-increasing values.

**Files modified:** `src/cdc.rs`, `src/version.rs`, `src/refresh.rs`, `src/dvm/diff.rs`, `src/lib.rs`, `src/config.rs`

---

### Phase 4: Distributed CDC — Worker-Side Triggers (~4 weeks)

**Goal:** Make CDC triggers fire on Citus worker nodes where DML actually executes, with change buffers accessible from the coordinator for refresh.

**P4.1: Branch CDC setup on placement**

In `setup_cdc_for_source()` at `src/api.rs` (L519):

| Placement | Trigger Strategy |
|-----------|-----------------|
| `Local` | Current behavior — create trigger on coordinator |
| `CitusReference` | Current behavior — triggers fire on coordinator for reference tables |
| `CitusDistributed` | Use `citus::run_on_all_nodes()` to create trigger function + trigger on every node |

**P4.2: Worker-side trigger creation for distributed tables**

Modify `create_change_trigger()` in `src/cdc.rs` (L37) to accept a `placement` parameter:

```rust
pub fn create_change_trigger(
    source_id: &SourceIdentifier,
    change_schema: &str,
    pk_columns: &[String],
    columns: &[(String, String)],
    placement: &TablePlacement,
) -> Result<String, PgStreamError>
```

For `CitusDistributed`:
1. Generate the trigger function SQL (same template, with `nextval()` instead of `pg_current_wal_lsn()`)
2. Wrap in `citus::run_on_all_nodes()` to execute on coordinator + all workers
3. The `CREATE TRIGGER` is also propagated via `run_on_all_nodes()`

**P4.3: Distributed change buffer tables**

For distributed sources, the change buffer table must exist on all workers where triggers fire:

1. Create `pg_stream_changes.changes_{stable_hash}` on coordinator
2. Call `SELECT create_distributed_table('pg_stream_changes.changes_{stable_hash}', 'seq_id')` to distribute the buffer by sequence ID
3. Workers write locally to their shard of the change buffer
4. The coordinator can query the entire buffer via normal SQL — Citus routes to all workers and unions results

**Why distributed table (not reference table):** Reference tables replicate every write to all nodes via 2PC, creating unacceptable overhead for high-frequency trigger writes. A distributed buffer localizes writes to the worker where DML occurred.

**P4.4: Update trigger existence checks**

`trigger_exists()` in `src/cdc.rs` (L570-L576) must check on workers for distributed tables:

```rust
pub fn trigger_exists(source_id: &SourceIdentifier, placement: &TablePlacement) -> bool {
    match placement {
        TablePlacement::CitusDistributed { .. } => {
            // Check on coordinator — if trigger exists here, it was propagated to workers
            local_trigger_exists(source_id)
        }
        _ => local_trigger_exists(source_id),
    }
}
```

**P4.5: Update cleanup for distributed change buffers**

`delete_consumed_changes()` in `src/cdc.rs` (L430) and the TRUNCATE path in `src/refresh.rs` (L617):
- Citus supports `TRUNCATE` on distributed tables — works as-is
- `DELETE ... WHERE seq_id > ... AND seq_id <=` also works via Citus routing

**P4.6: DDL event trigger handling**

In `src/hooks.rs` (L53): When Citus propagates schema changes to workers, the coordinator event trigger fires. The hook must detect if the affected table is distributed and rebuild triggers on workers via `run_on_all_nodes()`, not just locally.

**Files modified:** `src/cdc.rs`, `src/api.rs`, `src/hooks.rs`, `src/refresh.rs`

---

### Phase 5: Distributed Refresh — MERGE Rewrite (~4 weeks)

**Goal:** Make the refresh pipeline work when the ST storage table is distributed or when change buffers are distributed.

**P5.1: ST storage table placement decision**

When creating a ST, auto-select placement:

| Source Configuration | ST Placement | Rationale |
|---------------------|-------------|-----------|
| All sources local | `local` | Current behavior, no changes |
| Any source is reference, none distributed | `local` | Reference table data is on coordinator |
| Any source is distributed, ST estimated < 100K rows | `reference` | Small STs replicated everywhere for fast reads |
| Any source is distributed, ST estimated ≥ 100K rows | `distributed` | Large STs distributed by `__pgs_row_id` |

Add `st_placement TEXT` column to `pg_stream.pgs_stream_tables` in `src/lib.rs` (L80). Values: `'local'`, `'reference'`, `'distributed'`. Default: `'local'`.

**P5.2: Storage table distribution**

After `CREATE TABLE {pgs_schema}.{pgs_name}`, based on placement:

```sql
-- For reference STs:
SELECT create_reference_table('{pgs_schema}.{pgs_name}');

-- For distributed STs:
SELECT create_distributed_table('{pgs_schema}.{pgs_name}', '__pgs_row_id');
```

The unique index on `__pgs_row_id` serves as the distribution key.

**P5.3: Replace MERGE with INSERT ON CONFLICT + DELETE**

For `st_placement = 'distributed'`, replace the MERGE statement in `src/refresh.rs` (L165-L175) with two Citus-compatible statements:

```sql
-- Step 1: Delete rows that are removed or updated
DELETE FROM {st} WHERE __pgs_row_id IN (
    SELECT __pgs_row_id FROM ({delta_cte}) d WHERE d.__pgs_action = 'D'
);

-- Step 2: Insert new rows or update existing
INSERT INTO {st} ({columns})
SELECT {columns} FROM ({delta_cte}) d WHERE d.__pgs_action = 'I'
ON CONFLICT (__pgs_row_id) DO UPDATE SET
    {col1} = EXCLUDED.{col1},
    {col2} = EXCLUDED.{col2},
    ...;
```

This is fully supported by Citus when `__pgs_row_id` is the distribution column.

**P5.4: Extend `CachedMergeTemplate`**

In `src/refresh.rs` (L28-L52), add a variant for the INSERT ON CONFLICT + DELETE pattern:

```rust
pub struct CachedMergeTemplate {
    pub merge_sql: Option<String>,          // MERGE (local/reference)
    pub delete_sql: Option<String>,         // DELETE step (distributed)
    pub upsert_sql: Option<String>,         // INSERT ON CONFLICT step (distributed)
    pub is_deduplicated: bool,
    pub column_count: usize,
    pub source_oids: Vec<u32>,
    pub st_placement: String,
}
```

**P5.5: Update IVM output for distributed mode**

`src/dvm/mod.rs` must emit the two-statement form when `st_placement = 'distributed'`. The delta CTE chain itself is unchanged — only the final apply statement differs.

**P5.6: Distributed CTE execution concerns**

The delta CTE chain (generated by `src/dvm/operators/`) references `changes_{stable_hash}` tables. When these are Citus-distributed, the CTE executes on the coordinator which pulls data from workers.

**Performance concern:** Citus may not push down complex multi-CTE pipelines. Mitigation:
1. Profile with `EXPLAIN ANALYZE` in integration tests
2. If push-down fails, materialize the delta into a temp table before apply:
   ```sql
   CREATE TEMP TABLE __pgs_delta AS ({delta_cte});
   DELETE FROM {st} WHERE __pgs_row_id IN (SELECT __pgs_row_id FROM __pgs_delta WHERE __pgs_action = 'D');
   INSERT INTO {st} SELECT ... FROM __pgs_delta WHERE __pgs_action = 'I' ON CONFLICT ...;
   DROP TABLE __pgs_delta;
   ```

**P5.7: Fix row count estimates for distributed tables**

Update the `pg_class.reltuples` query in `src/refresh.rs` (L371-L393) for distributed tables:

```sql
-- For distributed tables, use citus_stat_statements or sum across shards:
SELECT sum(reltuples)::bigint AS table_size
FROM pg_dist_shard s
JOIN pg_class c ON c.oid = s.shardid
WHERE s.logicalrelid = {oid}::oid
```

Or use `SELECT count(*) FROM citus_shards WHERE table_name = ...` as a fallback.

**Files modified:** `src/refresh.rs`, `src/dvm/mod.rs`, `src/lib.rs`, `src/api.rs`

---

### Phase 6: Distributed Coordination (~3 weeks)

**Goal:** Replace node-local coordination mechanisms (shared memory, advisory locks) with cluster-aware alternatives.

**P6.1: Scheduler remains coordinator-only**

The background worker at `src/scheduler.rs` (L41) stays on the coordinator node. It discovers STs from the local `pg_stream.pgs_stream_tables` catalog and executes refreshes that Citus routes to workers as needed. No changes to worker registration.

**P6.2: Replace advisory locks with catalog-based locks**

Add a new lock table to `src/lib.rs`:

```sql
CREATE TABLE pg_stream.st_locks (
    pgs_id      BIGINT PRIMARY KEY REFERENCES pg_stream.pgs_stream_tables(pgs_id),
    locked_by  INT,            -- PID of lock holder
    locked_at  TIMESTAMPTZ
);
```

Lock acquisition (replaces `pg_try_advisory_lock` at `src/scheduler.rs` L281, L425):

```sql
INSERT INTO pg_stream.st_locks (pgs_id, locked_by, locked_at)
VALUES ($1, pg_backend_pid(), now())
ON CONFLICT (pgs_id) DO NOTHING
```

Returns 1 row if acquired, 0 if already locked. Release:

```sql
DELETE FROM pg_stream.st_locks WHERE pgs_id = $1 AND locked_by = pg_backend_pid()
```

Add a stale-lock cleanup: locks older than `pg_stream.lock_timeout` (default: 10 minutes) are automatically released by the scheduler loop.

**P6.3: Replace shared memory DAG signal with LISTEN/NOTIFY**

Replace `PgAtomic<AtomicU64>` DAG rebuild signal at `src/shmem.rs` (L37) with PostgreSQL's LISTEN/NOTIFY:

- `signal_dag_rebuild()` → `NOTIFY pg_stream_dag_rebuild`
- Scheduler loop → `LISTEN pg_stream_dag_rebuild` + poll with `pg_sleep_for()`

This works across all connections to the same database (coordinator). For multi-coordinator HA setups, only one coordinator is writable at a time, so LISTEN/NOTIFY is sufficient.

**P6.4: Conditional shared memory usage**

Keep `PgLwLock<PgStreamSharedState>` for `scheduler_pid` and `scheduler_running` — these are coordinator-local state that doesn't need cross-node visibility. Only replace the DAG rebuild signal.

Update `src/shmem.rs`:
```rust
pub fn signal_dag_rebuild() {
    // Use NOTIFY for Citus-safe signaling
    let _ = Spi::run("NOTIFY pg_stream_dag_rebuild");
    // Also update atomic for backward compat with local shmem consumers
    if is_shmem_available() {
        DAG_REBUILD_SIGNAL.get().fetch_add(1, Ordering::SeqCst);
    }
}
```

**Files modified:** `src/scheduler.rs`, `src/shmem.rs`, `src/lib.rs`

---

### Phase 7: Testing & Migration (~3 weeks)

**Goal:** Comprehensive testing against Citus clusters and safe migration for existing installations.

**P7.1: Citus test infrastructure**

Add `tests/e2e_citus_tests.rs` using `citusdata/citus:13.0` Docker image with Testcontainers:

```rust
// tests/e2e_citus_tests.rs
// Uses a 1-coordinator + 2-worker Citus cluster via Docker Compose or
// testcontainers with a custom Citus image that includes the extension
```

Add `tests/Dockerfile.e2e-citus` for building the extension against the Citus image:

```dockerfile
FROM citusdata/citus:13.0
# Install Rust, pgrx, build extension
COPY . /app
WORKDIR /app
RUN cargo pgrx package
# Copy artifacts into PG extension dir
```

**P7.2: Test matrix**

| Test | Sources | ST Placement | Exercises |
|------|---------|-------------|-----------|
| Local regression | Local tables | Local | All existing behavior — no regressions |
| Reference source | Citus reference table | Local | Triggers on coordinator, LSN frontier |
| Distributed source → local ST | Hash-distributed table | Local | Worker triggers, seq frontier, coordinator MERGE |
| Distributed source → reference ST | Hash-distributed table | Reference | Worker triggers, seq frontier, reference MERGE |
| Distributed source → distributed ST | Hash-distributed table | Distributed | Full pipeline: worker triggers, seq frontier, INSERT ON CONFLICT |
| Mixed sources | Distributed + reference | Local | Multi-mode frontier, mixed CDC |
| Concurrent DML + refresh | Distributed table | Distributed | Workers writing + coordinator refreshing |
| Schema change on distributed source | Distributed table | Local | DDL event trigger → rebuild triggers on workers |
| DROP STREAM TABLE cleanup | Distributed source | Distributed | Cleanup triggers on all workers, drop distributed buffer |
| Multi-ST sharing source | 2 STs on 1 distributed table | Distributed | Change buffer sharing, TRUNCATE safety |

**P7.3: SQL migration scripts**

Write `ALTER EXTENSION pg_stream UPDATE` migration that:

1. Add `source_stable_name TEXT` and `source_placement TEXT` columns to `pg_stream.pgs_dependencies`
2. Add `source_stable_name TEXT` column to `pg_stream.pgs_change_tracking`
3. Add `st_placement TEXT DEFAULT 'local'` column to `pg_stream.pgs_stream_tables`
4. Create `pg_stream.change_seq` sequence
5. Create `pg_stream.st_locks` table
6. Backfill `stable_name` from current OID-based data:
   ```sql
   UPDATE pg_stream.pgs_dependencies d SET source_stable_name = (
       SELECT pg_stream.stable_hash(n.nspname || '.' || c.relname)
       FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
       WHERE c.oid = d.source_relid
   );
   ```
7. Rename existing `changes_{oid}` tables to `changes_{stable_hash}`
8. Rebuild trigger functions with new naming

**P7.4: Documentation updates**

Update:
- `docs/ARCHITECTURE.md` — Citus architecture diagrams, placement modes
- `docs/CONFIGURATION.md` — New GUCs (if any), placement auto-selection behavior
- `docs/SQL_REFERENCE.md` — ST creation with distributed sources, new columns
- `README.md` — Citus compatibility section
- `INSTALL.md` — Citus cluster installation instructions

**Files modified/created:** `tests/e2e_citus_tests.rs`, `tests/Dockerfile.e2e-citus`, `sql/` migration files, `docs/*`

---

## 5. File Impact Summary

| File | Phase(s) | Change Scope |
|------|----------|-------------|
| `src/citus.rs` (new) | 1 | New module — detection utilities, ~200 lines |
| `src/lib.rs` | 1, 2, 3, 5, 6 | Catalog DDL additions, module registration |
| `src/cdc.rs` | 2, 3, 4 | Stable naming, dual-mode triggers, frontier mode |
| `src/api.rs` | 1, 2, 4, 5 | Placement detection, branched CDC setup, ST distribution |
| `src/catalog.rs` | 1, 2 | SourceIdentifier, placement in StDependency |
| `src/version.rs` | 2, 3 | VersionMarker enum, stable_name keys |
| `src/refresh.rs` | 2, 3, 5 | Placeholder format, seq-mode detection, MERGE rewrite |
| `src/dvm/diff.rs` | 2, 3 | Stable_name placeholders, frontier mode |
| `src/dvm/mod.rs` | 5 | Dual-mode output (MERGE vs INSERT ON CONFLICT) |
| `src/dvm/operators/scan.rs` | 2 | Stable naming for change table references |
| `src/dvm/operators/aggregate.rs` | 2 | Stable naming for change table references |
| `src/dvm/operators/recursive_cte.rs` | 2 | Stable naming for change table references |
| `src/scheduler.rs` | 6 | Catalog locks, LISTEN/NOTIFY |
| `src/shmem.rs` | 6 | NOTIFY-based DAG signal |
| `src/hooks.rs` | 4 | Worker trigger rebuild for distributed sources |
| `src/config.rs` | 3 | FrontierMode GUC (if needed) |
| `src/error.rs` | 1 | Citus-specific error variants |

---

## 6. Implementation Priority & Sequencing

```
Phase 1: Citus Detection           ← Foundation: all subsequent phases depend on this
    │
    ▼
Phase 2: Stable Naming             ← Breaking change — must be done early with migration
    │
    ▼
Phase 3: Logical Sequence          ← Unblocks distributed CDC
    │       Frontier
    ▼
Phase 4: Distributed CDC           ← Core distributed functionality
    │
    ▼
Phase 5: Distributed Refresh       ← Makes STs queryable across the cluster
    │       (MERGE rewrite)
    ▼
Phase 6: Distributed               ← Operational robustness
    │       Coordination
    ▼
Phase 7: Testing & Migration       ← Validation (also runs incrementally per phase)
```

Phases 1–2 can be shipped as a non-breaking release (Citus not required). Phase 2's stable naming improves pg_dump/restore even on single-node.

Phases 3–6 are Citus-specific and should ship together as a feature release.

Phase 7 testing runs incrementally throughout all phases.

---

## 7. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Citus distributed sequence performance under high-frequency triggers | Medium | High — if `nextval()` across nodes is slow, trigger overhead explodes | Benchmark early in Phase 3; fallback to worker-local sequences with coordinator merge |
| Complex CTE push-down failures in Citus | High | Medium — delta queries may run entirely on coordinator, pulling all data | Materialize delta into temp table before apply (Phase 5 P5.6) |
| Citus version compatibility (targeting Citus 13.x for PG 18) | Medium | High — Citus 13.x may not be released yet | Track Citus release schedule; test against Citus nightly builds |
| MERGE → INSERT ON CONFLICT semantic differences | Low | High — edge cases in concurrent updates | Thorough testing of concurrent DML + refresh (Phase 7) |
| Migration breaks existing single-node installs | Low | Critical — data loss for existing users | OID columns retained; migration is additive; rollback path via `ALTER EXTENSION ... UPDATE TO 'prev_version'` |
| `run_command_on_workers()` failure mid-setup | Medium | Medium — partial trigger creation across workers | Wrap in transaction where possible; add cleanup/retry logic |
| Advisory lock → catalog lock performance | Low | Low — catalog locks slightly slower than advisory | Advisory locks still used as fast path on non-Citus; catalog locks only for Citus |
| Reference table STs limited by replication overhead | Low | Low — only used for small STs | Document size recommendation; auto-selection threshold at 100K rows |

---

## 8. Open Questions

1. **Citus 13.x availability for PG 18:** Is Citus 13.x released and stable for PG 18.x? If not, what is the timeline?

2. **Distributed sequence semantics:** Does `nextval()` on a Citus-distributed sequence provide **strict monotonic ordering** across workers, or only uniqueness? If only uniqueness, the frontier ordering assumption breaks and we need an alternative (e.g., `(worker_id, local_seq)` composite ordering).

3. **Schema `pg_stream_changes` on workers:** Workers may not have the `pg_stream_changes` schema. The extension `CREATE SCHEMA` runs on coordinator only. Must use `run_on_all_nodes('CREATE SCHEMA IF NOT EXISTS pg_stream_changes')` during setup.

4. **Extension loading on workers:** Do workers need `shared_preload_libraries = 'pg_stream'`? The background worker is coordinator-only, but the trigger functions reference the extension. If workers don't load the extension, PL/pgSQL triggers (not C triggers) should work without it.

5. **Citus columnar storage:** Should ST storage tables support Citus columnar? Columnar doesn't support UPDATE/DELETE, so it's incompatible with incremental refresh. Document as unsupported.

---

## 9. Milestone Checkpoints

| Milestone | Phase | Deliverable | Verification |
|-----------|-------|-------------|-------------|
| M1: Citus detection works | 1 | `is_citus_available()` returns true on Citus cluster | Unit test with mocked catalog |
| M2: Stable naming deployed | 2 | All change buffers use `stable_hash` naming | Existing e2e tests pass with new naming |
| M3: Sequence frontier works | 3 | Trigger writes `seq_id` on distributed table | e2e test: INSERT on worker → change buffer has seq_id |
| M4: Worker CDC operational | 4 | Triggers fire on workers, changes visible from coordinator | e2e test: DML on distributed table → coordinator reads changes |
| M5: Distributed refresh works | 5 | Full INCR refresh pipeline on distributed ST | e2e test: create ST → mutate → refresh → verify results |
| M6: Coordination is cluster-safe | 6 | Scheduler uses catalog locks, NOTIFY signaling | e2e test: concurrent refresh attempts on Citus |
| M7: Migration from single-node | 7 | Existing single-node install upgrades cleanly | Migration test: pre-Citus data → upgrade → verify |
| M8: Full test suite green | 7 | All existing + new Citus tests pass | CI pipeline with Citus cluster |
