# Plan: Transactionally Updated Views (Immediate IVM)

Date: 2026-02-28
Status: PROPOSED
Last Updated: 2026-02-28

---

## 1. Overview

This plan proposes adding **immediate (transactional) incremental view
maintenance** to pg_trickle — updating stream tables within the **same
transaction** that modifies a base table. Today, all stream table refreshes
happen in separate transactions (either on a schedule or manually). This new
mode would complement the existing deferred (scheduled) refresh model and
enable pg_trickle to serve as a **drop-in replacement for pg_ivm** users.

### Terminology

| Term | Definition |
|------|-----------|
| **Deferred IVM** | Views are updated in a separate transaction (current pg_trickle model). |
| **Immediate IVM** | Views are updated in the same transaction as the base table DML (pg_ivm model). |
| **IMMV** | Incrementally Maintainable Materialized View (pg_ivm term). |
| **Stream Table** | pg_trickle's equivalent of a materialized view with IVM. |

### Goals

1. Allow stream tables to be updated **in the same transaction** as base table
   modifications, providing read-your-writes consistency.
2. Provide a **pg_ivm compatibility layer** so existing pg_ivm users can
   migrate with minimal changes.
3. Maintain backward compatibility — the current deferred/scheduled model
   remains the default.
4. Support the subset of SQL that pg_ivm supports in immediate mode, with a
   path to extending coverage using pg_trickle's broader operator support.

### Non-Goals (out of scope for Phase 1)

- Replacing the deferred refresh mode (it remains the default).
- Supporting immediate IVM for all SQL constructs pg_trickle supports
  (window functions, UNION/INTERSECT/EXCEPT, recursive CTEs, LATERAL, etc.
  will start as deferred-only).
- Multi-table concurrent write correctness without locking (pg_ivm also uses
  ExclusiveLock to serialize).

---

## 2. Background: How pg_ivm Works

### 2.1 Architecture

pg_ivm implements immediate IVM entirely through **statement-level AFTER
triggers** with **transition tables** (the `REFERENCING NEW TABLE AS ...
OLD TABLE AS ...` syntax). This is the key mechanism:

1. **BEFORE trigger** (`IVM_immediate_before`): Fires before each statement.
   Takes a snapshot of the pre-modification state. Tracks a count of before
   trigger invocations per view.

2. **AFTER trigger** (`IVM_immediate_maintenance`): Fires after each
   statement. Collects transition tables (`tg_oldtable`, `tg_newtable`) into
   in-memory tuplestores. When the last AFTER trigger fires (count matches
   BEFORE count), the actual delta computation and application runs.

3. **Delta computation**: The view's defining query is rewritten by:
   - Replacing each base table RTE with a subquery representing its
     "pre-update" state (using the snapshot from step 1 + the old transition
     table).
   - Computing "old delta" by substituting the modified table's RTE with the
     old transition table and executing the rewritten query.
   - Computing "new delta" by substituting with the new transition table.

4. **Delta application**: The deltas are applied to the IMMV via SPI:
   - Old delta → DELETE matching rows from the IMMV.
   - New delta → INSERT/UPDATE rows into the IMMV.
   - For aggregates: UPDATE existing group rows with incremental
     arithmetic (e.g., `SET sum = mv.sum + delta.sum`).

5. **Concurrency**: ExclusiveLock is taken on the IMMV during maintenance.
   In REPEATABLE READ/SERIALIZABLE, errors are raised if conflicts are
   detected.

### 2.2 pg_ivm Limitations

| Feature | pg_ivm Support | pg_trickle Support |
|---------|---------------|-------------------|
| Inner joins | ✓ | ✓ |
| Outer joins | ✓ (limited: simple equijoin only) | ✓ (full) |
| DISTINCT | ✓ | ✓ |
| count/sum/avg/min/max | ✓ | ✓ (+ more aggregates) |
| Window functions | ✗ | ✓ |
| UNION/INTERSECT/EXCEPT | ✗ | ✓ |
| DISTINCT ON | ✗ | ✓ (via rewrite) |
| GROUPING SETS/CUBE/ROLLUP | ✗ | ✓ (via rewrite) |
| Recursive CTEs | ✗ | ✓ |
| LATERAL subqueries | ✗ | ✓ |
| Subqueries in FROM | ✓ (simple) | ✓ |
| EXISTS subqueries | ✓ (simple) | ✓ |
| Views as base tables | ✗ | ✓ (via view inlining) |
| Partitioned tables | ✗ | ✓ |
| Cascading (ST→ST) | ✗ | ✓ |
| User-defined aggregates | ✗ | ✓ |
| Mutable functions | ✗ | ✓ (FULL mode) |
| HAVING | ✗ | ✓ |
| ORDER BY/LIMIT/OFFSET | ✗ | not applicable |

### 2.3 pg_ivm API Surface

```sql
-- Core functions
pgivm.create_immv(immv_name text, view_definition text) → bigint
pgivm.refresh_immv(immv_name text, with_data bool) → bigint
pgivm.get_immv_def(immv regclass) → text

-- Catalog
pgivm.pg_ivm_immv (immvrelid, viewdef, ispopulated, lastivmupdate)

-- Drop via: DROP TABLE immv_name
-- Rename via: ALTER TABLE immv_name RENAME TO new_name
```

---

## 3. Proposed Architecture for pg_trickle

### 3.1 New Refresh Mode: `IMMEDIATE`

Add a third refresh mode alongside `FULL` and `DIFFERENTIAL`:

```
refresh_mode = 'FULL' | 'DIFFERENTIAL' | 'IMMEDIATE'
```

When `refresh_mode = 'IMMEDIATE'`:
- Statement-level AFTER triggers with transition tables are installed on all
  base tables (instead of row-level CDC triggers).
- The trigger function computes and applies the delta **within the same
  transaction** as the base table DML.
- No change buffer tables are used (no `pgtrickle_changes.*`).
- No scheduler involvement — updates are synchronous.
- The stream table is always up-to-date within the current transaction.

### 3.2 Trigger Design

#### Current CDC Triggers (deferred mode)

```
AFTER INSERT OR UPDATE OR DELETE  (row-level)
  → writes to pgtrickle_changes.changes_<oid>
  → consumed later by scheduler/manual refresh
```

#### New IMMEDIATE Triggers

```
BEFORE INSERT OR UPDATE OR DELETE  (statement-level)
  → IVM_before: capture snapshot, increment before_count

AFTER INSERT OR UPDATE OR DELETE  (statement-level, with transition tables)
  → IVM_after: collect transition tables, when last trigger fires:
    1. Generate delta SQL from operator tree
    2. Apply delta to stream table
    3. Advance metadata

BEFORE TRUNCATE  (statement-level)
  → IVM_before

AFTER TRUNCATE  (statement-level)
  → Truncate or full-refresh the stream table
```

#### Implementation Strategy

**Option A: PL/pgSQL trigger calling a Rust pg_extern function**

The trigger body is a thin PL/pgSQL wrapper that calls a Rust function:

```sql
CREATE TRIGGER pgt_ivm_before_ins
  BEFORE INSERT ON source_table
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_before(<st_oid>, <lock_mode>);

CREATE TRIGGER pgt_ivm_after_ins
  AFTER INSERT ON source_table
  REFERENCING NEW TABLE AS __pgt_newtable
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_after(<st_oid>, <lock_mode>);
```

The Rust `pgt_ivm_before` and `pgt_ivm_after` functions handle all
the logic:

- `pgt_ivm_before`:
  - Register snapshot for pre-state visibility.
  - Increment per-view before-trigger counter.
  - Acquire appropriate lock on the stream table.

- `pgt_ivm_after`:
  - Collect transition tables into tuplestores.
  - Increment after-trigger counter.
  - When `before_count == after_count` (last trigger for this statement):
    - Generate delta SQL using the existing DVM engine.
    - Apply delta to stream table via SPI.
    - Update catalog metadata (frontier, data_timestamp).

**Option B: C-level trigger functions (like pg_ivm)**

Write the trigger functions in C (via pgrx `unsafe` blocks accessing
`pg_sys::TriggerData`) to directly access transition tables without the
PL/pgSQL overhead. This is what pg_ivm does.

**Recommendation: Option A for Phase 1, Option B as optimization.**

Option A is simpler, safer (fewer unsafe blocks), and leverages pgrx's
existing SPI infrastructure. Option B can be pursued later for performance
if the PL/pgSQL overhead is measurable. The critical performance factor is
the delta SQL execution, not the trigger dispatch overhead.

### 3.3 Delta Computation Strategy

#### Approach 1: Reuse Existing DVM Operator Tree (Recommended)

pg_trickle already has a sophisticated DVM engine that generates delta SQL
from an operator tree. For immediate mode, we adapt this:

1. **Pre-compute the delta SQL template** at `create_stream_table` time
   (already done for deferred mode via `CachedMergeTemplate`).

2. **In the AFTER trigger**, instead of reading from change buffer tables,
   we create **Ephemeral Named Relations (ENRs)** from the transition
   tables and rewrite the delta SQL to reference those ENRs.

3. The DVM's `Scan` operator delta would need an alternative code path:
   instead of `SELECT ... FROM pgtrickle_changes.changes_<oid> WHERE lsn
   BETWEEN $prev AND $new`, it would reference the ENR directly:
   `SELECT ... FROM __pgt_newtable` / `__pgt_oldtable`.

This approach reuses 90%+ of the existing DVM engine code.

#### Approach 2: Direct Query Rewriting (pg_ivm style)

Rewrite the defining query by replacing base table RTEs, similar to what
pg_ivm does. This would be a separate code path from the DVM engine.

**Not recommended** — duplicates logic and doesn't leverage pg_trickle's
existing operator tree infrastructure.

### 3.4 Transition Table Access

PostgreSQL's transition tables (`REFERENCING NEW TABLE AS ... OLD TABLE AS
...`) are only available in statement-level AFTER triggers. They provide
read-only access to the rows affected by the triggering statement.

Key considerations:

1. **Multi-table views**: If a view joins tables A and B, and A is modified,
   we need A's transition table plus B's current state. This is exactly how
   the current DVM delta works — join the delta of one side against the
   current state of the other.

2. **Self-joins**: If a table appears multiple times in the query, the same
   transition table is used for all occurrences. pg_ivm handles this with
   `rte_paths` tracking.

3. **Cascading**: If stream table B depends on stream table A, and A's base
   table is modified, first A must be updated (in the same trigger), then
   B must be updated using A's changes. This requires **ordering trigger
   execution** or handling cascades explicitly within the trigger function.

### 3.5 Concurrency and Locking

Following pg_ivm's proven approach:

| Scenario | Lock on Stream Table | Rationale |
|----------|---------------------|-----------|
| Single base table, no agg/distinct, INSERT only | RowExclusiveLock | Safe because inserts don't conflict |
| Multiple base tables, or agg/distinct | ExclusiveLock | Prevents concurrent conflicting updates |
| TRUNCATE | ExclusiveLock | Full replacement |

In **READ COMMITTED** isolation:
- The lock ensures serial maintenance of the stream table.
- After acquiring the lock, push a fresh snapshot to see concurrent commits.

In **REPEATABLE READ / SERIALIZABLE**:
- If the lock can't be acquired immediately, raise an error.
- If a concurrent transaction already updated the view, raise an error
  to prevent inconsistency.

### 3.6 Catalog Changes

#### New column in `pgt_stream_tables`

```sql
ALTER TABLE pgtrickle.pgt_stream_tables
  ADD COLUMN refresh_mode text NOT NULL DEFAULT 'DIFFERENTIAL';
-- Already exists, but add 'IMMEDIATE' as a valid value
```

The existing `refresh_mode` column already stores `'FULL'` or
`'DIFFERENTIAL'`. Add `'IMMEDIATE'` as a third option.

#### New catalog table for IMMEDIATE mode metadata

```sql
CREATE TABLE pgtrickle.pgt_ivm_state (
    pgt_id         bigint  PRIMARY KEY REFERENCES pgtrickle.pgt_stream_tables(pgt_id),
    lock_mode      text    NOT NULL DEFAULT 'EXCLUSIVE',
    -- 'EXCLUSIVE' or 'ROW_EXCLUSIVE' (auto-determined)
    last_update_xid xid8,   -- pg_ivm compat: last transaction that updated
    is_populated    boolean NOT NULL DEFAULT false
);
```

#### Change tracking differences

For IMMEDIATE mode stream tables:
- **No** change buffer tables (`pgtrickle_changes.changes_<oid>`) are
  created.
- **No** CDC row-level triggers are created.
- Statement-level triggers with transition tables are created instead.
- **No** frontier tracking (there's no LSN-based change window).
- `data_timestamp` is updated to `now()` after each trigger-based refresh.

### 3.7 API Changes

#### Updated `create_stream_table`

```sql
pgtrickle.create_stream_table(
    name          text,
    query         text,
    schedule      text DEFAULT '1m',
    refresh_mode  text DEFAULT 'DIFFERENTIAL',
    initialize    bool DEFAULT true
) → void
```

When `refresh_mode = 'IMMEDIATE'`:
- `schedule` is ignored (set to NULL internally).
- The stream table is populated immediately (full refresh).
- Statement-level IVM triggers are created on all base tables.
- The query is validated against IMMEDIATE mode restrictions (see §3.8).

#### Updated `alter_stream_table`

```sql
pgtrickle.alter_stream_table(
    name          text,
    schedule      text DEFAULT NULL,
    refresh_mode  text DEFAULT NULL,
    status        text DEFAULT NULL
) → void
```

Allow switching between `'DIFFERENTIAL'` and `'IMMEDIATE'`:
- `DIFFERENTIAL → IMMEDIATE`: Drop CDC triggers, create IVM triggers, drop
  change buffer tables, do a full refresh to ensure consistency.
- `IMMEDIATE → DIFFERENTIAL`: Drop IVM triggers, create CDC triggers, create
  change buffer tables, do a full refresh.
- `FULL → IMMEDIATE` or `IMMEDIATE → FULL`: Similar migration.

#### New: `refresh_stream_table` in IMMEDIATE mode

For IMMEDIATE mode tables, `refresh_stream_table()` performs a **full
refresh** (like pg_ivm's `refresh_immv(name, true)`). This is useful to
re-sync after disabling/re-enabling immediate mode.

### 3.8 Query Restrictions for IMMEDIATE Mode

Phase 1 supports the same query subset as pg_ivm, plus pg_trickle's auto-
rewrite passes. Unsupported constructs in IMMEDIATE mode will error at
creation time with a clear message suggesting `'DIFFERENTIAL'` mode instead.

**Supported in Phase 1:**

- `SELECT ... FROM table` (simple scan)
- `JOIN` (inner, left, right, full outer — with equijoin restriction lifted
  in later phases)
- `WHERE` (filter)
- `GROUP BY` with `count`, `sum`, `avg`, `min`, `max`
- `DISTINCT`
- Simple subqueries in `FROM`
- `EXISTS` subqueries in `WHERE`
- Simple `WITH` (non-recursive CTEs)
- Views (via existing auto-rewrite view inlining)
- `DISTINCT ON` (via existing auto-rewrite)
- `GROUPING SETS/CUBE/ROLLUP` (via existing auto-rewrite)

**Deferred to later phases:**

- Window functions
- `UNION`/`INTERSECT`/`EXCEPT`
- Recursive CTEs
- `LATERAL` subqueries and functions
- Scalar subqueries in SELECT
- User-defined aggregates (needs verification of incremental formulas)
- Cascading IMMEDIATE stream tables (ST depending on another IMMEDIATE ST)

---

## 4. pg_ivm Compatibility Layer

### 4.1 Compatibility Functions

Provide a `pgivm` schema with wrapper functions:

```sql
CREATE SCHEMA IF NOT EXISTS pgivm;

-- create_immv: wraps create_stream_table with IMMEDIATE mode
CREATE FUNCTION pgivm.create_immv(
    immv_name text,
    view_definition text
) RETURNS bigint AS $$
DECLARE
    row_count bigint;
BEGIN
    -- Parse immv_name to extract schema if qualified
    PERFORM pgtrickle.create_stream_table(
        name := immv_name,
        query := view_definition,
        schedule := NULL,
        refresh_mode := 'IMMEDIATE',
        initialize := true
    );
    -- Return row count
    EXECUTE format('SELECT count(*) FROM %I', immv_name) INTO row_count;
    RETURN row_count;
END;
$$ LANGUAGE plpgsql;

-- refresh_immv: wraps refresh_stream_table
CREATE FUNCTION pgivm.refresh_immv(
    immv_name text,
    with_data boolean
) RETURNS bigint AS $$
DECLARE
    row_count bigint;
BEGIN
    IF with_data THEN
        PERFORM pgtrickle.refresh_stream_table(immv_name);
        EXECUTE format('SELECT count(*) FROM %I', immv_name) INTO row_count;
        RETURN row_count;
    ELSE
        -- Disable immediate maintenance
        PERFORM pgtrickle.alter_stream_table(
            name := immv_name,
            status := 'SUSPENDED'
        );
        EXECUTE format('TRUNCATE %I', immv_name);
        RETURN 0;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- get_immv_def: wraps existing catalog
CREATE FUNCTION pgivm.get_immv_def(immv regclass) RETURNS text AS $$
    SELECT st.defining_query
    FROM pgtrickle.pgt_stream_tables st
    WHERE st.pgt_relid = immv;
$$ LANGUAGE sql STABLE;
```

### 4.2 Compatibility Catalog View

```sql
CREATE VIEW pgivm.pg_ivm_immv AS
SELECT
    st.pgt_relid AS immvrelid,
    st.defining_query AS viewdef,
    st.is_populated AS ispopulated,
    NULL::xid8 AS lastivmupdate  -- or read from pgt_ivm_state
FROM pgtrickle.pgt_stream_tables st
WHERE st.refresh_mode = 'IMMEDIATE';
```

### 4.3 Drop/Rename Compatibility

pg_ivm uses `DROP TABLE` and `ALTER TABLE RENAME` for IMMVs. pg_trickle
stream tables are already regular tables, so `DROP TABLE` would need to
be intercepted to also clean up catalog entries and triggers.

Options:
1. **object_access_hook** (what pg_ivm uses): Intercept DROP TABLE events
   and clean up IMMV catalog entries. pg_trickle already has DDL event
   triggers (`_on_sql_drop`) that do this.
2. **FK cascading + event triggers** (current pg_trickle approach): The
   existing `_on_sql_drop` event trigger already detects when stream table
   storage tables are dropped and marks them for cleanup.

The existing mechanism should work. We may need to add explicit `DROP TABLE`
support (currently users use `pgtrickle.drop_stream_table()`).

### 4.4 Migration Guide for pg_ivm Users

```sql
-- pg_ivm:
SELECT pgivm.create_immv('my_view', 'SELECT a, sum(b) FROM t GROUP BY a');

-- pg_trickle (native):
SELECT pgtrickle.create_stream_table(
    'my_view',
    'SELECT a, sum(b) FROM t GROUP BY a',
    refresh_mode := 'IMMEDIATE'
);

-- pg_trickle (compat layer):
SELECT pgivm.create_immv('my_view', 'SELECT a, sum(b) FROM t GROUP BY a');
-- Works identically!
```

---

## 5. Implementation Phases

### Phase 1: Core IMMEDIATE Mode (MVP)

**Goal:** Single-table & multi-table immediate IVM with aggregates and JOIN.

1. **Add IMMEDIATE refresh mode to catalog and API**
   - Accept `'IMMEDIATE'` in `create_stream_table` and `alter_stream_table`.
   - Store in `pgt_stream_tables.refresh_mode`.
   - Validate query restrictions.

2. **Implement statement-level IVM triggers**
   - `pgtrickle.pgt_ivm_before(st_oid, lock_mode)` — BEFORE trigger function.
   - `pgtrickle.pgt_ivm_after(st_oid, lock_mode)` — AFTER trigger function
     with transition table access.
   - Wire trigger creation into `create_stream_table` when
     `refresh_mode = 'IMMEDIATE'`.
   - In-memory state tracking (hash table keyed by stream table OID) for
     before/after counts, snapshots, transition tuplestores.

3. **Adapt DVM delta computation for transition tables**
   - New `DeltaSource` enum: `ChangeBuffer { lsn_range }` vs
     `TransitionTable { old_tuplestore, new_tuplestore }`.
   - Modify `Scan` operator to generate delta SQL from ENRs when in
     IMMEDIATE mode.
   - Reuse all other operators as-is (Filter, Project, Join, Aggregate,
     etc. don't care where the scan delta comes from).

4. **Delta application via existing explicit DML path**
   - Reuse the existing trigger_delete/update/insert templates from
     `CachedMergeTemplate`.
   - Instead of materializing into `__pgt_delta_{pgt_id}`, materialize
     from the ENR-based delta SQL.

5. **Handle TRUNCATE**
   - Truncate the stream table, or full-refresh for aggregate views
     without GROUP BY.

6. **Basic concurrency: ExclusiveLock on all IMMEDIATE stream tables**
   - pg_ivm's approach. Sufficient for correctness.

7. **Tests**
   - Unit tests for delta computation with transition tables.
   - E2E tests: INSERT/UPDATE/DELETE on base tables, verify stream table
     updates immediately.
   - Concurrent transaction tests.

### Phase 2: pg_ivm Compatibility Layer

**Goal:** Drop-in replacement for pg_ivm users.

1. **Implement `pgivm.*` wrapper functions** (see §4.1).
2. **Implement `pgivm.pg_ivm_immv` catalog view** (see §4.2).
3. **Support `DROP TABLE` for IMMEDIATE stream tables** via event triggers.
4. **Support `ALTER TABLE RENAME`** to update catalog entries.
5. **Automatic index creation** on IMMEDIATE stream tables (matching
   pg_ivm's behavior: index on GROUP BY columns, DISTINCT columns, or
   base table PKs).
6. **Add `__ivm_count__` hidden column** for views with DISTINCT or
   aggregates (or use existing `__pgt_row_id`).
7. **Migration documentation and test suite** comparing pg_ivm and
   pg_trickle behavior.

### Phase 3: Extended Query Support

**Goal:** Support more SQL features in IMMEDIATE mode.

1. **Window functions** — partition-based recomputation (already supported in
   deferred mode).
2. **UNION/INTERSECT/EXCEPT** — merge deltas from branches.
3. **LATERAL subqueries and functions**.
4. **Cascading IMMEDIATE stream tables** — when ST A updates, immediately
   propagate to downstream ST B (within the same transaction).
5. **Optimized locking** — use RowExclusiveLock when safe (single-table,
   no agg/distinct, INSERT-only).

### Phase 4: Performance Optimization

1. **C-level trigger functions** (Option B from §3.2) to eliminate PL/pgSQL
   overhead.
2. **Delta SQL template caching** — pre-compile IMMEDIATE mode delta SQL
   (already done for deferred mode).
3. **Prepared statement reuse** — keep SPI prepared statements across trigger
   invocations within the same transaction.
4. **Benchmarking suite** — compare pg_trickle IMMEDIATE vs pg_ivm vs
   deferred refresh on standard workloads.

---

## 6. Technical Deep-Dive

### 6.1 Trigger Registration

For each base table referenced by an IMMEDIATE stream table, create:

```sql
-- BEFORE triggers (one per DML type)
CREATE TRIGGER pgt_ivm_before_ins_{st_oid}
  BEFORE INSERT ON {source_table}
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_before('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_before_upd_{st_oid}
  BEFORE UPDATE ON {source_table}
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_before('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_before_del_{st_oid}
  BEFORE DELETE ON {source_table}
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_before('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_before_trunc_{st_oid}
  BEFORE TRUNCATE ON {source_table}
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_before('{st_oid}', 'true');

-- AFTER triggers (with transition tables)
CREATE TRIGGER pgt_ivm_after_ins_{st_oid}
  AFTER INSERT ON {source_table}
  REFERENCING NEW TABLE AS __pgt_newtable
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_after('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_after_upd_{st_oid}
  AFTER UPDATE ON {source_table}
  REFERENCING OLD TABLE AS __pgt_oldtable NEW TABLE AS __pgt_newtable
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_after('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_after_del_{st_oid}
  AFTER DELETE ON {source_table}
  REFERENCING OLD TABLE AS __pgt_oldtable
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_after('{st_oid}', '{lock_mode}');

CREATE TRIGGER pgt_ivm_after_trunc_{st_oid}
  AFTER TRUNCATE ON {source_table}
  FOR EACH STATEMENT
  EXECUTE FUNCTION pgtrickle.pgt_ivm_after('{st_oid}', 'true');
```

### 6.2 In-Memory State Management

```rust
/// Per-session hash table for tracking IVM trigger state.
/// Similar to pg_ivm's mv_trigger_info.
struct IvmTriggerState {
    /// Keyed by stream table OID.
    entries: HashMap<Oid, IvmTriggerEntry>,
}

struct IvmTriggerEntry {
    /// OID of the stream table being maintained.
    st_oid: Oid,
    /// Snapshot taken just before the first modification.
    snapshot: Option<pg_sys::Snapshot>,
    /// Count of BEFORE trigger invocations.
    before_count: u32,
    /// Count of AFTER trigger invocations.
    after_count: u32,
    /// Transition tables collected from AFTER triggers.
    tables: Vec<IvmTriggerTable>,
    /// Whether any old tuples were seen (DELETE/UPDATE).
    has_old: bool,
    /// Whether any new tuples were seen (INSERT/UPDATE).
    has_new: bool,
}

struct IvmTriggerTable {
    /// OID of the modified base table.
    table_oid: Oid,
    /// Tuplestores for deleted/updated-from tuples.
    old_tuplestores: Vec<*mut pg_sys::Tuplestorestate>,
    /// Tuplestores for inserted/updated-to tuples.
    new_tuplestores: Vec<*mut pg_sys::Tuplestorestate>,
}
```

This state is stored in `TopTransactionContext` memory and is automatically
freed on transaction abort/commit. An `AtAbort_IVM` callback (registered
via `RegisterXactCallback`) handles cleanup on abort.

### 6.3 Delta SQL Generation for Transition Tables

The key adaptation is in the `Scan` operator's delta generation:

**Current (deferred mode):**
```sql
-- Delta SQL reads from change buffer table
SELECT action, new_col1, new_col2, old_col1, old_col2
FROM pgtrickle_changes.changes_12345
WHERE lsn > $prev_lsn AND lsn <= $new_lsn
```

**New (immediate mode):**
```sql
-- Delta SQL reads from transition table ENRs
-- For INSERT:
SELECT 'I' AS action, col1, col2 FROM __pgt_newtable

-- For DELETE:
SELECT 'D' AS action, col1, col2 FROM __pgt_oldtable

-- For UPDATE:
SELECT 'D' AS action, col1, col2 FROM __pgt_oldtable
UNION ALL
SELECT 'I' AS action, col1, col2 FROM __pgt_newtable
```

The transition tables are registered as Ephemeral Named Relations (ENRs)
in the query environment, making them accessible from SPI queries.

### 6.4 Cascading IMMEDIATE Stream Tables

When ST B depends on ST A (both IMMEDIATE), and a base table of A is
modified:

1. A's AFTER trigger fires, computes delta, updates A.
2. A's update fires B's AFTER trigger (because A is B's base table).
3. B's AFTER trigger computes delta using A's transition (the rows
   that were inserted/deleted/updated in A), and updates B.

This requires that:
- Stream tables be recognized as valid "base tables" for IMMEDIATE triggers.
- The trigger on A fires B's IVM triggers via the DML it performs on A.
- Topological ordering is implicitly handled by trigger nesting.

**Risk:** Deep cascading chains may exceed PostgreSQL's trigger nesting
limit or cause performance issues. This is deferred to Phase 3.

### 6.5 Transaction Lifecycle Integration

```
User issues: INSERT INTO base_table VALUES (...)

PostgreSQL:
  1. Execute INSERT
  2. Fire BEFORE statement triggers
     → pgt_ivm_before: register snapshot, increment before_count
  3. Insert rows (row-level triggers fire if any)
  4. Fire AFTER statement triggers
     → pgt_ivm_after: collect transition table
     → before_count == after_count? Yes.
       → CommandCounterIncrement() [make inserted rows visible]
       → Acquire ExclusiveLock on stream table
       → Generate delta SQL (ENR-based)
       → Execute delta via SPI: DELETE old rows, INSERT/UPDATE new rows
       → Update catalog: data_timestamp, last_update_xid
  5. Return to user — stream table is already updated
```

---

## 7. Comparison with Current Architecture

| Aspect | Deferred (current) | Immediate (proposed) |
|--------|-------------------|---------------------|
| **When updates happen** | In scheduler or manual refresh (separate transaction) | In same transaction as DML |
| **Change capture** | Row-level AFTER triggers → change buffer tables | Statement-level AFTER triggers with transition tables |
| **Delta source** | Change buffer tables with LSN ranges | In-memory transition tuplestores (ENRs) |
| **Consistency** | Eventually consistent (staleness = schedule interval) | Strongly consistent (read-your-writes) |
| **Write overhead** | ~5-15% per DML (trigger writes to buffer table) | ~10-50%+ per DML (full delta computation inline) |
| **Read overhead** | None | None |
| **Concurrency** | No locking on stream table during DML | ExclusiveLock on stream table during DML |
| **Batching** | Changes accumulate, applied in bulk | Applied per-statement |
| **Query support** | Full pg_trickle operator set | Subset (Phase 1: pg_ivm equivalent) |
| **Cascading** | Scheduler handles topological order | Trigger nesting handles order |
| **Background worker** | Required (scheduler) | Not required |

---

## 8. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|-----------|
| **Write performance impact** | High | Clearly document tradeoffs. Default remains deferred. Users opt-in. |
| **Lock contention** | High | ExclusiveLock may block concurrent readers. Consider AccessShareLock compatibility. Document isolation level implications. |
| **Trigger nesting depth** | Medium | Limit cascade depth. PostgreSQL's `max_stack_depth` is the bound. |
| **Transition table memory** | Medium | Large DML may cause memory pressure. Document `work_mem` tuning. pg_ivm has the same limitation. |
| **Unsafe code in trigger functions** | Medium | Wrap all `pg_sys::*` calls in safe abstractions. Thorough review of SAFETY comments. |
| **Snapshot management complexity** | High | Carefully follow pg_ivm's proven snapshot lifecycle. Test under all isolation levels. |
| **Query restriction validation** | Low | Clear error messages pointing users to DIFFERENTIAL mode. |
| **Compatibility edge cases** | Medium | Comprehensive test suite comparing pg_ivm and pg_trickle behavior. |

---

## 9. Open Questions

1. **Should IMMEDIATE mode support the `__pgt_row_id` column?**
   pg_ivm doesn't use content-hash IDs — it matches rows by all columns
   (for non-aggregate) or by GROUP BY keys (for aggregates). We could
   either:
   - (a) Keep `__pgt_row_id` for consistency with deferred mode.
   - (b) Omit it for IMMEDIATE mode and match by column values (pg_ivm
     compat). This means IMMEDIATE stream tables have a different schema
     than DIFFERENTIAL ones.

2. **Should we support mixed mode?** E.g., a stream table that is normally
   IMMEDIATE but can be temporarily switched to DEFERRED for bulk loads
   (like pg_ivm's `refresh_immv(name, false)` + `refresh_immv(name, true)`
   pattern).

3. **How to handle `ALTER TABLE` on base tables of IMMEDIATE stream tables?**
   Currently, DDL event triggers set `needs_reinit`. For IMMEDIATE mode,
   should we immediately reinitialize (full refresh + recreate triggers)?

4. **Should the compatibility layer live in the pg_trickle extension or as a
   separate SQL file?** Keeping it in-extension simplifies installation but
   adds namespace complexity.

5. **Trigger naming conflicts**: If a base table already has pg_ivm triggers,
   what happens? We should detect this and advise the user.

---

## 10. Success Criteria

1. **Functional**: INSERT/UPDATE/DELETE on a base table immediately updates
   all IMMEDIATE stream tables within the same transaction. `SELECT` from
   the stream table after the DML (in the same transaction) returns
   up-to-date results.

2. **pg_ivm compatibility**: The `pgivm.create_immv()` wrapper produces
   behavior indistinguishable from pg_ivm for all supported query types.
   A pg_ivm test suite ported to pg_trickle should pass.

3. **Performance**: Write overhead for IMMEDIATE mode is within 2x of
   pg_ivm for equivalent queries. Deferred mode is not affected.

4. **Reliability**: No data corruption under concurrent transactions.
   Proper behavior under all isolation levels (READ COMMITTED, REPEATABLE
   READ, SERIALIZABLE).

5. **Observability**: IMMEDIATE stream tables appear in `pgtrickle.pgt_status`
   with `refresh_mode = 'IMMEDIATE'` and accurate `data_timestamp`.

---

## 11. References

- **pg_ivm source**: https://github.com/sraoss/pg_ivm
- **PostgreSQL transition tables**: https://www.postgresql.org/docs/current/trigger-definition.html
- **DBSP**: Budiu et al., "DBSP: Automatic Incremental View Maintenance" (VLDB 2023)
- **Gupta & Mumick**: "Maintenance of Materialized Views: Problems, Techniques, and Applications" (1995)
- **pg_trickle ADR-001**: Triggers as default CDC mechanism
- **pg_trickle ADR-003**: Query differentiation via operator tree
- **pg_trickle ADR-006**: Explicit DML for user triggers

---

## 12. ADR Reference

This plan would result in the following new ADR:

### ADR-008: Immediate (Transactional) IVM Mode

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Category** | IVM Engine |
| **Date** | 2026-02-28 |

**Decision:** Add an `IMMEDIATE` refresh mode that uses statement-level
AFTER triggers with transition tables to update stream tables in the same
transaction as base table DML. This provides read-your-writes consistency
and enables pg_ivm compatibility.

**Options Considered:**
1. Statement-level triggers with transition tables (chosen — matches pg_ivm's
   proven approach)
2. Row-level triggers with deferred constraint triggers (rejected — no
   transition table access, complex)
3. Custom executor hooks (rejected — too invasive, fragile across PG versions)
4. Logical decoding within transaction (rejected — not possible; WAL is only
   visible after commit)

**Key Points:**
- Complementary to existing deferred mode, not a replacement
- Reuses DVM engine with ENR-based delta source
- ExclusiveLock for concurrency safety (pg_ivm's approach)
- Phase 1 targets pg_ivm feature parity; later phases extend coverage
