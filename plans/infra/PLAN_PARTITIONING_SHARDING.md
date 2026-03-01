# PLAN_PARTITIONING_SHARDING.md — PostgreSQL Partitioning & Sharding Compatibility

**Status:** Plan  
**Date:** 2026-03-01  
**Related:** [PLAN_CITUS.md](PLAN_CITUS.md) · [PLAN_TIMESCALEDB.md](../ecosystem/PLAN_TIMESCALEDB.md) · [REPORT_TRIGGERS_VS_REPLICATION.md](../sql/REPORT_TRIGGERS_VS_REPLICATION.md)

---

## 1. Executive Summary

This report analyses how pg_trickle interacts with PostgreSQL's native table
partitioning (declarative partitioning, available since PG 10) and external
sharding solutions (Citus, pg_partman). It covers three scenarios:

1. **Partitioned source tables** — source tables referenced in stream table
   defining queries are partitioned.
2. **Partitioned stream table storage** — the materialized storage table
   backing a stream table is itself partitioned.
3. **Distributed/sharded deployments** — the database spans multiple nodes
   (Citus, foreign-data-wrapper sharding, read replicas).

**Key findings:**

- **Native partitioning of source tables works today** with minor caveats.
  PostgreSQL 13+ propagates row-level triggers from the partitioned parent to
  child partitions, so CDC captures all DML transparently. The extension already
  logs an info message (F13) confirming this.
- **Partitioned storage tables are not supported** and would require
  non-trivial changes to the storage table creation, MERGE/delta application,
  and row ID management paths.
- **Distributed sharding (Citus)** is a much larger problem with a dedicated
  plan already in [PLAN_CITUS.md](PLAN_CITUS.md). The core issues (OID
  locality, WAL LSN incomparability, trigger propagation) are architectural.
- The most impactful near-term improvement is **validating and documenting**
  the native partitioning story, and adding targeted tests.

---

## 2. Background: PostgreSQL Partitioning

### 2.1. Declarative Partitioning (PG 10+)

PostgreSQL supports declarative partitioning via `PARTITION BY`:

```sql
CREATE TABLE orders (
    id         BIGSERIAL,
    created_at TIMESTAMPTZ NOT NULL,
    customer_id INT,
    total      NUMERIC
) PARTITION BY RANGE (created_at);

CREATE TABLE orders_2025 PARTITION OF orders
    FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
CREATE TABLE orders_2026 PARTITION OF orders
    FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
```

Partition strategies: `RANGE`, `LIST`, `HASH`. Sub-partitioning is supported.

### 2.2. Key Properties for pg_trickle

| Property | Behaviour | Impact |
|----------|-----------|--------|
| **OID** | Parent table has its own OID; each child partition has a separate OID | CDC triggers are keyed by OID — must use parent OID |
| **Triggers** | PG 13+: row-level triggers on parent fire for DML routed to any partition | CDC works transparently |
| **Triggers (PG 11–12)** | Triggers on parent do NOT fire for partition-routed DML | CDC would miss all changes — effectively broken |
| **TRUNCATE** | `TRUNCATE parent` truncates all partitions; fires statement-level trigger on parent | Our TRUNCATE capture trigger works |
| **DDL propagation** | `ALTER TABLE parent ADD COLUMN` propagates to all partitions | DDL hooks see the parent's OID — reinit works |
| **Partition DDL** | `ALTER TABLE parent ATTACH/DETACH PARTITION` fires `ddl_command_end` | Hook fires but no column change → no action needed |
| **Primary keys** | PK must include all partition key columns | pk_hash computation works (resolves PK via parent OID) |
| **Statistics** | `pg_class.reltuples` on parent is the sum of child `reltuples` (PG 14+) | Adaptive FULL/DIFFERENTIAL ratio works correctly |
| **WAL** | Each partition generates its own WAL records | Logical replication must include all partitions in publication |
| **Indexes** | Partitioned indexes are virtual; each partition has its own | No impact (we don't read source indexes) |

### 2.3. Partition Management Extensions

- **pg_partman** — Automated partition creation/retention. Compatible: it
  manages DDL on child partitions, which doesn't affect the parent's schema.
  Dropping old partitions is equivalent to `DROP TABLE` on a child — the parent
  OID remains valid.
- **TimescaleDB hypertables** — Transparent time-based partitioning. See
  [PLAN_TIMESCALEDB.md](../ecosystem/PLAN_TIMESCALEDB.md) for detailed analysis.
  Hypertables are partitioned tables with additional metadata; triggers on the
  parent propagate to chunks.

---

## 3. Scenario 1: Partitioned Source Tables

### 3.1. Current Status — Works (PG 13+)

When a user creates a stream table over a partitioned source:

```sql
SELECT pgtrickle.create_stream_table(
    'daily_totals',
    $$SELECT date_trunc('day', created_at) AS day,
             sum(total) AS total
      FROM orders
      GROUP BY 1$$,
    schedule := '5 minutes'
);
```

The following happens:

1. **Source resolution** — `extract_source_relations()` resolves `orders` to
   the parent's OID. Child partition OIDs are NOT returned because the parser
   sees only the parent.

2. **Relkind detection** — `warn_source_table_properties()` detects
   `relkind = 'p'` and emits F13 info message.

3. **CDC trigger creation** — `create_change_trigger()` creates the trigger
   on the parent table. PostgreSQL 13+ automatically propagates the trigger
   to all existing and future child partitions.

4. **Change capture** — When DML hits any child partition, the trigger fires
   with `NEW`/`OLD` rows, writing to `pgtrickle_changes.changes_<parent_oid>`.
   The `pg_current_wal_lsn()` call in the trigger correctly reflects the WAL
   position of the child partition's write.

5. **PK resolution** — `resolve_pk_columns()` queries `pg_constraint` on the
   parent OID. For partitioned tables, the PK constraint is on the parent
   (and includes partition key columns). This works correctly.

6. **Column resolution** — `resolve_source_column_defs()` queries
   `pg_attribute` on the parent OID. All partitions share the same column
   set. Works correctly.

7. **Full refresh** — `SELECT ... FROM orders` naturally reads all partitions
   via partition pruning. Works correctly.

8. **Differential refresh** — The delta query reads from
   `pgtrickle_changes.changes_<parent_oid>`. Since all partition DML writes to
   the same buffer (keyed by parent OID), the delta is complete.

### 3.2. Caveats

| Caveat | Severity | Details |
|--------|----------|---------|
| **PG 11–12 incompatibility** | N/A | pg_trickle targets PG 18 only; not relevant |
| **ATTACH PARTITION with existing data** | Medium | `ALTER TABLE orders ATTACH PARTITION orders_2027 ...` does NOT fire row-level triggers for existing rows in the newly attached partition. The stream table will miss these rows until the next FULL refresh or reinitialization. |
| **DETACH PARTITION** | Low | `ALTER TABLE orders DETACH PARTITION orders_2024 ...` removes the partition. Future DML on the detached table won't fire the parent's trigger. Existing change buffer data for already-captured rows is fine. DDL hooks should detect the ALTER TABLE and consider reinit. |
| **Very large partition counts** | Low | With hundreds of partitions, the trigger is installed on each child. Trigger overhead is per-row and per-partition, but the trigger function is shared. No scalability concern observed up to ~500 partitions in PG benchmarks. |
| **WAL mode CDC** | Medium | For logical replication, the publication must include all partitions. `CREATE PUBLICATION ... FOR TABLE orders` in PG 13+ uses the `publish_via_partition_root` option to group all child WAL records under the parent table. `wal_decoder.rs` must set this option when creating publications for partitioned sources. |

### 3.3. ATTACH PARTITION Gap

This is the most significant gap. When a partition with pre-existing data is
attached to a partitioned source table:

```sql
-- This table already has 1M rows:
CREATE TABLE orders_2027 (LIKE orders INCLUDING ALL);
INSERT INTO orders_2027 SELECT ... ;

-- Attach it — no row-level triggers fire for existing rows:
ALTER TABLE orders ATTACH PARTITION orders_2027
    FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');
```

The 1M existing rows are now visible via `SELECT * FROM orders` but were
never captured by the CDC trigger. The stream table is stale.

**Detection:** The DDL event trigger (`_on_ddl_end`) fires for
`ALTER TABLE ... ATTACH PARTITION`. The `command_tag` is `ALTER TABLE` and the
`objid` is the parent table's OID. The hook should detect that the parent is a
tracked source and mark downstream STs for reinitialize.

**Current behaviour:** The DDL hook currently checks for column changes (ADD/
DROP/ALTER COLUMN). An ATTACH PARTITION doesn't change columns, so it may not
trigger reinit. This needs verification and potentially a targeted check for
the `ATTACH PARTITION` subcommand.

**Recommended fix:** In `handle_ddl_command()`, when the command_tag is
`ALTER TABLE` and the target is a partitioned source, additionally check
`pg_event_trigger_ddl_commands()` for partition-related subcommands. If
ATTACH PARTITION is detected, force `needs_reinit = true`.

### 3.4. WAL Mode: Publication Configuration

When transitioning from trigger to WAL mode for a partitioned source, the
publication must be configured correctly:

```sql
-- Current code in wal_decoder.rs creates:
CREATE PUBLICATION pgtrickle_<oid> FOR TABLE <table_name>;

-- For partitioned tables, this should use:
CREATE PUBLICATION pgtrickle_<oid>
    FOR TABLE <table_name>
    WITH (publish_via_partition_root = true);
```

The `publish_via_partition_root` option (PG 13+) ensures that changes to child
partitions are published under the parent table's identity, matching the
trigger-mode behavior where all changes appear in a single buffer keyed by
the parent OID.

Without this option, each child partition publishes its changes under its own
OID, and the decoder would need to map child OIDs back to the parent — a
significant complication.

---

## 4. Scenario 2: Partitioned Stream Table Storage

### 4.1. Current Status — Not Supported

Stream table storage tables are created as plain heap tables:

```sql
CREATE TABLE <schema>.<name> (
    __pgt_row_id BIGINT NOT NULL,
    col1 TYPE,
    col2 TYPE,
    ...
);
CREATE UNIQUE INDEX ... ON <schema>.<name> (__pgt_row_id);
```

There is no mechanism to partition the storage table.

### 4.2. Why Users Might Want This

- **Large materialized views** — When a stream table materializes billions of
  rows (e.g., time-series aggregations), partitioning the storage table by
  time could enable partition pruning on downstream queries and efficient
  retention management.
- **TTL / retention** — Users could drop old partitions of a stream table
  rather than running expensive DELETE operations.
- **Query performance** — Partition pruning on the storage table accelerates
  analytical queries that filter by the partition key.

### 4.3. Challenges

| Challenge | Severity | Details |
|-----------|----------|---------|
| **Storage table creation** | Medium | `api.rs` creates the storage table via `CREATE TABLE`. Would need to accept an optional `PARTITION BY` clause from the user and generate child partitions or rely on pg_partman. |
| **MERGE/delta application** | High | The differential refresh uses `DELETE` + `INSERT` keyed by `__pgt_row_id`. With partitioning, the partition key must be part of the row or derivable from it. The MERGE target would need to be the parent table, and PostgreSQL would route rows to the correct partition. This works transparently for INSERT but DELETE requires the partition key to be in the WHERE clause for partition pruning (otherwise it scans all partitions). |
| **Row ID uniqueness** | Medium | `__pgt_row_id` is a hash, not monotonically increasing. A `UNIQUE INDEX` on a partitioned table requires the partition key in the index. The unique constraint would become `(__pgt_row_id, <partition_key>)`, which weakens the uniqueness guarantee to per-partition scope. |
| **Full refresh (TRUNCATE + INSERT)** | Low | `TRUNCATE` on a partitioned table truncates all partitions. Works correctly. |
| **Reinitialize** | Medium | Currently drops and recreates the storage table. Would need to preserve the partition structure, or the user would need to re-specify it. |
| **User triggers** | Low | User triggers on the storage parent propagate to partitions. No issue. |

### 4.4. Potential Design

If implemented, partitioned storage could be an opt-in feature:

```sql
SELECT pgtrickle.create_stream_table(
    'daily_totals',
    $$SELECT date_trunc('day', created_at) AS day,
             sum(total) AS total
      FROM orders GROUP BY 1$$,
    schedule := '5 minutes',
    partition_by := 'RANGE (day)',   -- new parameter
    partition_spec := '{
        "auto_create": true,
        "retention": "2 years",
        "interval": "1 month"
    }'
);
```

The extension would:
1. Create the parent storage table with `PARTITION BY RANGE (day)`.
2. Use pg_partman or custom logic to auto-create child partitions.
3. Modify delta application to include the partition key in DELETE clauses.
4. Adjust the `__pgt_row_id` unique index to include the partition key.
5. Optionally manage retention (drop partitions older than threshold).

**Recommendation:** Defer this to a future release. The complexity is moderate
and the user base requesting partitioned storage tables should be established
first. Users needing partitioned storage today can create a partitioned table
manually and use a full-refresh-only stream table.

---

## 5. Scenario 3: Distributed Sharding (Citus)

### 5.1. Summary

Distributed sharding via Citus is **fundamentally different** from native
partitioning. While native partitioning keeps all data on a single PostgreSQL
instance with transparent query routing, Citus distributes data across
multiple independent PostgreSQL nodes.

A comprehensive plan exists in [PLAN_CITUS.md](PLAN_CITUS.md). The key
incompatibilities are:

| Component | Native Partitioning | Citus Distributed |
|-----------|--------------------|--------------------|
| **OIDs** | Parent OID is stable; child OIDs are local | OIDs differ across coordinator and workers |
| **WAL** | Single WAL stream | Independent WAL per node |
| **Triggers** | Propagate from parent to children | Do NOT propagate to worker shards |
| **SPI** | Single instance, full catalog access | Coordinator has catalog; workers have partial |
| **MERGE** | Fully supported | Limited support; cross-shard joins problematic |
| **Background workers** | Single scheduler | One scheduler per node or coordinator-only |
| **Advisory locks** | Instance-wide | Node-local |

### 5.2. Impact on pg_trickle Components

| Component | Native Partitioning Impact | Citus Impact |
|-----------|---------------------------|--------------|
| CDC triggers (`cdc.rs`) | Works — triggers propagate | Broken — triggers don't propagate to workers |
| Change buffers | Single buffer per parent OID | Need distributed or coordinator-aggregated buffers |
| WAL decoder (`wal_decoder.rs`) | Needs `publish_via_partition_root` | Need multi-node slot management |
| Frontier/LSN (`version.rs`) | Single WAL, single LSN timeline | Incomparable LSNs across nodes |
| Delta SQL (`dvm/`) | No change needed | Cross-shard join complications |
| MERGE application (`refresh.rs`) | Works transparently | Need INSERT ON CONFLICT + DELETE |
| DAG/scheduler (`dag.rs`, `scheduler.rs`) | No change | Need coordinator-only or multi-node coordination |
| DDL hooks (`hooks.rs`) | Works — events fire on parent | Only fires on coordinator |
| Shared memory (`shmem.rs`) | No change | Node-local; need LISTEN/NOTIFY |

### 5.3. Recommendation

Native PostgreSQL partitioning and Citus sharding should be treated as **two
entirely separate features** with different timelines:

1. **Native partitioning** — Near-term; mostly works today. Needs testing,
   ATTACH PARTITION detection, and WAL publication configuration.
2. **Citus** — Long-term; architectural changes needed. Follow the phased
   plan in [PLAN_CITUS.md](PLAN_CITUS.md).

---

## 6. Scenario 4: Foreign Data Wrapper (FDW) Sharding

### 6.1. Overview

Some deployments use `postgres_fdw` to create a "sharded" view across multiple
PostgreSQL instances:

```sql
CREATE FOREIGN TABLE remote_orders (...)
    SERVER remote_server OPTIONS (table_name 'orders');
```

Or with partitioned foreign tables (PG 14+):

```sql
CREATE TABLE orders (...) PARTITION BY RANGE (region);
CREATE FOREIGN TABLE orders_us PARTITION OF orders
    FOR VALUES IN ('US')
    SERVER us_server OPTIONS (table_name 'orders');
CREATE FOREIGN TABLE orders_eu PARTITION OF orders
    FOR VALUES IN ('EU')
    SERVER eu_server OPTIONS (table_name 'orders');
```

### 6.2. Compatibility

| Aspect | Status | Details |
|--------|--------|---------|
| **Source resolution** | Works | Foreign tables have OIDs in the local catalog |
| **CDC triggers** | Broken | Row-level triggers cannot be created on foreign tables (`ERROR: "..." is not a table or view`) |
| **WAL mode** | Broken | Foreign tables don't generate local WAL |
| **Full refresh** | Works | `SELECT * FROM foreign_table` executes the remote query |
| **Partition mix** | Partial | A partitioned table with some local and some foreign children: triggers fire only on local partitions; foreign partition DML is invisible to CDC |

### 6.3. Recommendation

Foreign-table sources should be restricted to `FULL` refresh mode only. The
extension should detect `relkind = 'f'` (foreign table) during source
resolution and emit a warning that differential refresh is not available.

For partitioned tables with a mix of local and foreign children, the safest
approach is to require `FULL` refresh mode, since CDC cannot capture changes
from foreign partitions.

---

## 7. Test Coverage Gaps

Current test coverage for partitioning scenarios:

| Scenario | Covered? | Notes |
|----------|----------|-------|
| Partitioned source — basic CRUD | No | Need E2E test with RANGE partitioned source |
| Partitioned source — differential refresh | No | Verify delta capture across partitions |
| Partitioned source — partition pruning in full refresh | No | Verify optimizer uses pruning |
| ATTACH PARTITION with existing data | No | Verify reinit or staleness detection |
| DETACH PARTITION | No | Verify DDL hook fires |
| pg_partman automatic partition creation | No | Verify triggers propagate to new partitions |
| Source with sub-partitioning | No | Two-level partitioning |
| Partitioned source — WAL mode CDC | No | Verify `publish_via_partition_root` |
| Foreign table source — full refresh only | No | Verify trigger creation error handling |
| Mixed local/foreign partitions | No | Verify correct warning or mode restriction |

**Recommended test additions:** Create a new `tests/e2e_partitioning_tests.rs`
file with tests covering the scenarios above.

---

## 8. Action Items

### Near-Term (Current Release)

| # | Action | Component | Effort |
|---|--------|-----------|--------|
| 1 | Add E2E tests for partitioned source tables (basic CRUD, differential refresh) | `tests/` | 1–2 days |
| 2 | Verify ATTACH PARTITION triggers reinit; add detection if missing | `src/hooks.rs` | 1 day |
| 3 | Set `publish_via_partition_root = true` in WAL publication creation for partitioned sources | `src/wal_decoder.rs` | Half day |
| 4 | Detect foreign table sources (`relkind = 'f'`) and restrict to FULL mode | `src/api.rs` | Half day |
| 5 | Document partitioned source table support in user docs | `docs/` | Half day |

### Medium-Term (Future Release)

| # | Action | Component | Effort |
|---|--------|-----------|--------|
| 6 | Explore partitioned storage table support (opt-in) | `src/api.rs`, `src/refresh.rs` | 2–3 weeks |
| 7 | Handle DETACH PARTITION explicitly in DDL hooks | `src/hooks.rs` | 1 day |
| 8 | Test with pg_partman automated partition management | `tests/` | 1–2 days |

### Long-Term

| # | Action | Component | Effort |
|---|--------|-----------|--------|
| 9 | Citus distributed table support | All modules | ~6 months (see [PLAN_CITUS.md](PLAN_CITUS.md)) |
| 10 | FDW-sharded source support with remote CDC | New module | Research needed |

---

## 9. Summary Matrix

| Deployment Pattern | Source Tables | Storage Tables | Status |
|--------------------|--------------|----------------|--------|
| **Native partitioning (RANGE/LIST/HASH)** | Works (PG 13+) | Not supported | Ready with caveats |
| **Sub-partitioning** | Works (same as above) | Not supported | Untested |
| **pg_partman managed** | Works (triggers propagate to new partitions) | Not supported | Untested |
| **TimescaleDB hypertables** | Works (see [PLAN_TIMESCALEDB.md](../ecosystem/PLAN_TIMESCALEDB.md)) | Not supported | Research |
| **Citus distributed** | Not supported | Not supported | See [PLAN_CITUS.md](PLAN_CITUS.md) |
| **Citus reference tables** | Works (coordinator-local triggers) | Possible | Untested |
| **FDW foreign tables** | Full refresh only | N/A | Needs implementation |
| **Mixed local + foreign partitions** | Full refresh only | N/A | Needs implementation |

---

## Appendix A: PostgreSQL Partition Trigger Propagation

Starting in PostgreSQL 13, row-level triggers created on a partitioned parent
table are automatically cloned to all existing and newly created child partitions.
The trigger fires with the child partition's `TG_TABLE_NAME` and `TG_RELID`,
but the trigger function is the same.

Key behavior details:

```sql
-- Create trigger on parent
CREATE TRIGGER cdc_trigger
    AFTER INSERT OR UPDATE OR DELETE ON orders
    FOR EACH ROW EXECUTE FUNCTION capture_changes();

-- PostgreSQL automatically creates matching triggers on:
--   orders_2025, orders_2026, orders_2027, ...

-- New partition created later:
CREATE TABLE orders_2028 PARTITION OF orders
    FOR VALUES FROM ('2028-01-01') TO ('2029-01-01');
-- Trigger is automatically cloned to orders_2028
```

Within the trigger function for pg_trickle, the `TG_RELID` is the **child
partition's OID**, not the parent's. However, since our trigger function is
generated with the parent OID hardcoded (e.g., `pg_trickle_cdc_fn_<parent_oid>`),
the change buffer table name is derived from the parent OID, and all partition
DML is correctly routed to the same buffer table.

## Appendix B: Partition Key in Row ID

For partitioned storage tables (Scenario 2), the `__pgt_row_id` uniqueness
guarantee requires special handling:

```
Current:  UNIQUE INDEX ON storage (__pgt_row_id)
          → Works for unpartitioned tables

With partitioning:
          UNIQUE INDEX ON storage (__pgt_row_id, <partition_key>)
          → Uniqueness is per-partition only
          → Two rows in different partitions could have the same __pgt_row_id

Alternative:
          Use __pgt_row_id as a composite of (original_hash, partition_key_hash)
          → Globally unique but changes hash semantics
```

This is one of the key design decisions that would need resolution before
implementing partitioned storage tables.
