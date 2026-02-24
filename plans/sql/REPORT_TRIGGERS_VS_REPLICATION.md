# Triggers vs Logical Replication for CDC in pg_stream

**Status:** Evaluation Report  
**Date:** 2026-02-24  
**Context:** [ADR — Triggers Instead of Logical Replication](../adrs/adr-triggers-instead-of-logical-replication.md) · [PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md)

---

## Executive Summary

pg_stream uses **row-level AFTER triggers** to capture changes on source tables.
This report evaluates the trigger-based approach against **logical replication**
(WAL-based CDC) across five dimensions: correctness, performance, operations,
and two end-user features — user-defined triggers on stream tables and logical
replication subscriptions from stream tables.

**Conclusion:** Triggers remain the correct choice for the current scope. The
single-transaction atomicity requirement is a hard constraint that logical
replication cannot meet without significant API compromises. The two end-user
features (user triggers and logical replication FROM stream tables) are both
achievable without changing the CDC mechanism.

---

## 1. Background

### Current Architecture

CDC triggers on each tracked source table write typed per-column rows into
per-table buffer tables (`pgstream_changes.changes_<oid>`). Each buffer row
captures:

| Column | Purpose |
|---|---|
| `change_id` | BIGSERIAL ordering within a source |
| `lsn` | `pg_current_wal_lsn()` at trigger time |
| `action` | `'I'` / `'U'` / `'D'` |
| `pk_hash` | Content hash of PK columns (optional) |
| `new_<col>` | Per-column NEW values (INSERT/UPDATE) |
| `old_<col>` | Per-column OLD values (UPDATE/DELETE) |

A covering B-tree index `(lsn, pk_hash, change_id) INCLUDE (action)` supports
the differential refresh's LSN-range scan.

### The Atomicity Constraint

`create_stream_table()` performs DDL (CREATE TABLE) and DML (catalog inserts)
before setting up CDC. `pg_create_logical_replication_slot()` **cannot execute
inside a transaction that has already performed writes**. This makes
single-transaction atomic creation impossible with logical replication — the
decisive factor in the original ADR.

---

## 2. Comparison Matrix

### 2.1 Correctness & Transactional Safety

| Aspect | Triggers | Logical Replication |
|---|---|---|
| Atomic creation | ✅ Same transaction as DDL+catalog | ❌ Slot creation requires separate transaction |
| Change visibility | ✅ Immediate (same transaction) | ⚠️ Asynchronous (after COMMIT + WAL decode) |
| TRUNCATE capture | ❌ Row-level triggers not fired | ✅ WAL emits TRUNCATE since PG 11 |
| Transaction ordering | ✅ Change buffer rows ordered by LSN | ✅ WAL stream preserves commit order |
| Crash recovery | ✅ Buffer tables are WAL-logged; no orphan state | ⚠️ Slot survives crash but may need re-sync |
| Schema change handling | ✅ DDL event hooks rebuild trigger in-place | ⚠️ Requires slot re-creation or output plugin awareness |

**Key insight:** The TRUNCATE gap is the most significant correctness
limitation of the trigger approach. A statement-level `AFTER TRUNCATE` trigger
that marks downstream STs for automatic FULL refresh would close this gap
without changing the CDC architecture (see §5 Recommendation 3).

### 2.2 Performance

| Metric | Triggers | Logical Replication |
|---|---|---|
| Per-row write overhead | ~2–4 μs (narrow INSERT) to ~5–15 μs (wide UPDATE) | ~0 (WAL writes happen regardless) |
| Expected throughput reduction | 1.5–5× on tracked source tables | None on source tables |
| Write amplification | 2× (source WAL + buffer table WAL + index) | 1× (only source WAL) |
| Change buffer storage | Heap table + index per source | WAL segments (shared, recycled) |
| Sequence contention | BIGSERIAL per buffer (lightweight) | N/A |
| Throughput ceiling | ~5,000 writes/sec (estimated) | WAL throughput (much higher) |
| Decoding CPU cost | N/A | Non-trivial; output plugin runs in WAL sender |
| Zero-change refresh | ~3 ms (EXISTS check on empty buffer) | ~3 ms (no pending WAL changes) |

**Key insight:** Trigger overhead is **synchronous** — every committing
transaction pays the cost. For applications with moderate write rates
(<5,000 writes/sec) this is acceptable. For high-throughput OLTP workloads,
logical replication's zero write-side overhead is a significant advantage.

### 2.3 Operational Complexity

| Aspect | Triggers | Logical Replication |
|---|---|---|
| PostgreSQL configuration | None required | `wal_level = logical` + restart |
| Managed PG compatibility | ✅ Works everywhere | ⚠️ Some providers restrict `wal_level` |
| WAL retention risk | None (buffer tables are independent) | Slots prevent WAL cleanup; disk exhaustion risk |
| Slot management | N/A | Create, monitor, drop; orphan detection |
| `max_replication_slots` | N/A | Must be sized for number of tracked sources |
| `REPLICA IDENTITY` config | N/A | Required on all tracked source tables |
| Monitoring | Buffer table row counts | Slot lag, WAL retention, decode rate |
| Extension dependencies | None | Output plugin (`pgoutput`, `wal2json`, or custom) |
| Upgrade path | `CREATE OR REPLACE FUNCTION` | Slot protocol version compatibility |

**Key insight:** Triggers are operationally simpler by a wide margin. Logical
replication introduces a class of failure modes (stuck slots, WAL bloat,
replica identity misconfiguration) that require dedicated monitoring and
operational runbooks.

### 2.4 Feature: User Triggers on Stream Tables

This addresses end-user triggers on the **output** stream tables, not CDC
triggers on source tables.

| Aspect | Current (Trigger CDC) | With Logical Replication CDC |
|---|---|---|
| Feasibility | ✅ Achievable via `session_replication_role` | ✅ Same mechanism applies |
| Refresh suppression | `SET LOCAL session_replication_role = 'replica'` | Same |
| Post-refresh notification | `NOTIFY pg_stream_refresh` with metadata | Same |
| MERGE firing pattern | DELETE+INSERT (not UPDATE); must be suppressed | Same — refresh mechanism is independent of CDC |

**Key insight:** User trigger support on stream tables is **orthogonal to the
CDC mechanism**. The solution (`session_replication_role = 'replica'` during
refresh) works identically regardless of whether changes are captured via
triggers or logical replication. The existing plan in
[PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md) is sound.

**Caveat:** `session_replication_role = 'replica'` may interact with logical
replication **publishing** from stream tables (see §2.5). This needs
verification before implementation.

### 2.5 Feature: Logical Replication FROM Stream Tables

This addresses end-users **subscribing** to stream table changes via
PostgreSQL's built-in logical replication.

| Aspect | Status | Notes |
|---|---|---|
| Basic publishing | ✅ Works today | STs are regular heap tables; `CREATE PUBLICATION` works |
| `__pgs_row_id` column | ⚠️ Replicated by default | Use column list in PUBLICATION to exclude, or document as usable PK |
| Differential refresh | ✅ DELETE+INSERT via MERGE are replicated | Subscriber sees individual DELETEs and INSERTs, not UPDATEs |
| Full refresh | ✅ TRUNCATE + INSERT replicated | Subscriber needs `replica_identity` set; receives TRUNCATE + mass INSERT |
| `REPLICA IDENTITY` | Needs configuration | `__pgs_row_id` could serve as unique index for identity |

#### The `session_replication_role` Conflict

If the refresh engine sets `session_replication_role = 'replica'` to suppress
user triggers (Phase 1 of the user-trigger plan), this may also suppress
**publication of the DML to logical replication subscribers**. When a session
is in `replica` mode, PostgreSQL treats it as a replication subscriber — DML
performed in that session may not be forwarded to downstream subscribers
(depending on the publication's `publish_via_partition_root` and the
subscriber's `origin` setting).

**This is a potential conflict** between the two features. Options:

| Option | User Triggers Suppressed? | Replication Published? | Drawback |
|---|---|---|---|
| `session_replication_role = 'replica'` | ✅ Yes | ❌ May not be published | Breaks logical replication from STs |
| `ALTER TABLE ... DISABLE TRIGGER USER` | ✅ Yes | ✅ Yes | Requires `ACCESS EXCLUSIVE` lock |
| `pg_stream.suppress_user_triggers` GUC → `DISABLE TRIGGER USER` only when needed | ✅ Configurable | ✅ Yes | Lock overhead; crash safety concern (ENABLE on recovery) |
| `tgisinternal` flag manipulation | ✅ Yes | ✅ Yes | Non-portable; catalog-level hack |

**Recommended resolution:** Use `ALTER TABLE ... DISABLE TRIGGER USER` within
a SAVEPOINT, restoring on error. The `ACCESS EXCLUSIVE` lock is brief (only
held for the catalog update, not the entire refresh). If the user has enabled
both user triggers AND logical replication on a stream table, this is the only
approach that supports both simultaneously. If neither feature is in use, skip
the overhead entirely.

---

## 3. TRUNCATE: The Gap and How to Close It

The TRUNCATE limitation is the most commonly cited drawback of trigger-based
CDC. PostgreSQL does not fire row-level triggers for TRUNCATE because TRUNCATE
operates at the file level (O(1)) — there are no individual rows to enumerate.

### Current Behavior

1. User runs `TRUNCATE source_table`
2. CDC trigger does **not** fire — change buffer remains empty
3. Scheduler sees zero changes → `NO_DATA` → stream table is **stale**
4. Stream table shows data from rows that no longer exist

### Proposed Fix: Statement-Level AFTER TRUNCATE Trigger

PostgreSQL supports statement-level `AFTER TRUNCATE` triggers. While they
provide no `OLD` row data, they can mark downstream stream tables for
reinitialization:

```sql
CREATE TRIGGER pg_stream_truncate_<oid>
  AFTER TRUNCATE ON <source_table>
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgstream.on_source_truncated('<source_oid>');
```

The trigger function would:
1. Look up all stream tables that depend on this source
2. Mark them `needs_reinit = true` in the catalog
3. Cascade transitively to downstream STs

This closes the TRUNCATE gap without changing the CDC architecture. The next
scheduler cycle would trigger a FULL refresh automatically.

**Effort estimate:** ~2–4 hours (trigger creation in `cdc.rs`, PL/pgSQL or
Rust function for `on_source_truncated`, cascade logic reuse from `hooks.rs`).

---

## 4. Migration Path: Trigger → Logical Replication

The original ADR notes that the buffer table schema and downstream IVM pipeline
are **decoupled** from the capture mechanism. If future requirements demand
logical replication (>5,000 writes/sec, cross-database CDC), migration is
isolated to the CDC layer:

### Phase A: Hybrid Creation

1. `create_stream_table()` continues using triggers for atomic creation
2. After first successful full refresh, a background worker creates a
   replication slot and transitions to WAL-based capture
3. Trigger is dropped; buffer table continues to be populated from WAL decode

### Phase B: Steady-State WAL Capture

1. Background worker runs a logical decoding consumer per tracked source
2. WAL changes are decoded and written to the same buffer table schema
3. Downstream pipeline (DVM, MERGE, frontier) is unchanged
4. TRUNCATE events are captured natively from WAL

### Prerequisites

- `wal_level = logical` (must be documented as optional upgrade path)
- `REPLICA IDENTITY` on tracked sources (auto-configured or user-managed)
- Custom output plugin or `pgoutput` + column mapping
- Slot health monitoring (WAL retention alerts, orphan cleanup)

**Effort estimate:** 3–5 weeks for a production-quality implementation.

---

## 5. Recommendations

### Recommendation 1: Keep Trigger-Based CDC

The atomicity constraint is decisive. Operational simplicity and zero-config
deployment are strong advantages for an early-stage extension. The performance
ceiling (~5,000 writes/sec) is adequate for the target use cases.

### Recommendation 2: Implement User Trigger Suppression

Follow the [PLAN_USER_TRIGGERS.md](PLAN_USER_TRIGGERS.md) plan with one
modification: **test `session_replication_role = 'replica'` interaction with
PUBLICATION before committing to it**. If it blocks publication, use
`ALTER TABLE ... DISABLE TRIGGER USER` within a SAVEPOINT instead.

### Recommendation 3: Add TRUNCATE Capture Trigger

Add a statement-level `AFTER TRUNCATE` trigger on each tracked source table
that marks downstream STs for reinitialization. This closes the most
significant usability gap without changing the CDC architecture.

### Recommendation 4: Document Logical Replication FROM Stream Tables

Add documentation and examples for `CREATE PUBLICATION` on stream tables,
including:
- Column filtering to exclude `__pgs_row_id`
- `REPLICA IDENTITY` configuration using `__pgs_row_id` as unique index
- Behavior during FULL vs DIFFERENTIAL refresh
- Interaction with user trigger suppression

### Recommendation 5: Benchmark Trigger Overhead

Execute the benchmark plan in
[TRIGGERS_OVERHEAD.md](../performance/TRIGGERS_OVERHEAD.md) to establish
data-driven thresholds for the logical replication migration crossover point.

---

## 6. Decision Log

| # | Decision | Rationale |
|---|---|---|
| D1 | Keep triggers for CDC on source tables | Atomicity, zero-config, adequate performance |
| D2 | User triggers on STs are orthogonal to CDC choice | `session_replication_role` / `DISABLE TRIGGER USER` works with either approach |
| D3 | Logical replication FROM STs works today | Regular heap tables; needs documentation, not code |
| D4 | TRUNCATE gap is closable with statement-level trigger | Low effort, high impact |
| D5 | Logical replication CDC is a viable future migration | Buffer table schema is decoupled; migration is isolated |
