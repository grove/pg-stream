# REPORT_DB_SCHEMA_STABILITY.md — Database Schema Stability Assessment

**Author:** pg_trickle team  
**Date:** 2026-02-25  
**Status:** Assessment / Pre-1.0 Readiness  
**Scope:** All database objects created by the pg_trickle extension

---

## 1. Executive Summary

Before releasing pg_trickle 1.0, we must audit every database object the
extension creates — schemas, tables, views, functions, triggers, event
triggers, GUCs, naming conventions, and internal column names — and decide
which surfaces are **public API** (stable after 1.0) and which are
**internal** (can evolve with migration scripts).

This document:

1. Inventories all database objects and classifies them as public or internal
2. Identifies **bugs and inconsistencies** that must be fixed before 1.0
3. Proposes **pre-1.0 changes** to reduce future breaking changes
4. Defines an **upgrade/migration strategy** using PostgreSQL's built-in
   `ALTER EXTENSION ... UPDATE` mechanism
5. Establishes **stability contracts** for each object category

---

## 2. Complete Object Inventory

### 2.1 Schemas

| Schema | Purpose | API Level |
|--------|---------|-----------|
| `pgtrickle` | Catalog tables, SQL functions, views | **Public** |
| `pgtrickle_changes` | Change buffer tables (configurable via GUC) | **Internal** |

### 2.2 Catalog Tables

#### `pgtrickle.pgt_stream_tables` — Core ST metadata

| Column | Type | Constraints | API |
|--------|------|-------------|-----|
| `pgt_id` | BIGSERIAL | PRIMARY KEY | Internal (surrogate) |
| `pgt_relid` | OID | NOT NULL UNIQUE | Internal |
| `pgt_name` | TEXT | NOT NULL | Public (via views) |
| `pgt_schema` | TEXT | NOT NULL | Public (via views) |
| `defining_query` | TEXT | NOT NULL | Public (via `explain_st`) |
| `original_query` | TEXT | | Internal |
| `schedule` | TEXT | | Public |
| `refresh_mode` | TEXT | NOT NULL DEFAULT `'DIFFERENTIAL'` | Public |
| `status` | TEXT | NOT NULL DEFAULT `'INITIALIZING'` | Public |
| `is_populated` | BOOLEAN | NOT NULL DEFAULT FALSE | Public |
| `data_timestamp` | TIMESTAMPTZ | | Public |
| `frontier` | JSONB | | Internal |
| `last_refresh_at` | TIMESTAMPTZ | | Public |
| `consecutive_errors` | INT | NOT NULL DEFAULT 0 | Public |
| `needs_reinit` | BOOLEAN | NOT NULL DEFAULT FALSE | Internal |
| `auto_threshold` | DOUBLE PRECISION | | Internal |
| `last_full_ms` | DOUBLE PRECISION | | Internal |
| `functions_used` | TEXT[] | | Internal |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT `now()` | Public |
| `updated_at` | TIMESTAMPTZ | NOT NULL DEFAULT `now()` | Public |

#### `pgtrickle.pgt_dependencies` — Source dependency edges

| Column | Type | Constraints | API |
|--------|------|-------------|-----|
| `pgt_id` | BIGINT | FK → `pgt_stream_tables`, PK | Internal |
| `source_relid` | OID | PK | Internal |
| `source_type` | TEXT | CHECK IN ('TABLE','STREAM_TABLE','VIEW') | Internal |
| `columns_used` | TEXT[] | | Internal |
| `column_snapshot` | JSONB | | Internal |
| `schema_fingerprint` | TEXT | | Internal |
| `cdc_mode` | TEXT | DEFAULT 'TRIGGER', CHECK IN ('TRIGGER','TRANSITIONING','WAL') | Internal |
| `slot_name` | TEXT | | Internal |
| `decoder_confirmed_lsn` | PG_LSN | | Internal |
| `transition_started_at` | TIMESTAMPTZ | | Internal |

#### `pgtrickle.pgt_refresh_history` — Audit log

| Column | Type | Constraints | API |
|--------|------|-------------|-----|
| `refresh_id` | BIGSERIAL | PRIMARY KEY | Internal |
| `pgt_id` | BIGINT | NOT NULL | Internal |
| `data_timestamp` | TIMESTAMPTZ | NOT NULL | Public (via `get_refresh_history`) |
| `start_time` | TIMESTAMPTZ | NOT NULL | Public |
| `end_time` | TIMESTAMPTZ | | Public |
| `action` | TEXT | CHECK IN (...) | Public |
| `rows_inserted` | BIGINT | DEFAULT 0 | Public |
| `rows_deleted` | BIGINT | DEFAULT 0 | Public |
| `error_message` | TEXT | | Public |
| `status` | TEXT | CHECK IN (...) | Public |
| `initiated_by` | TEXT | CHECK IN (...) | Public |
| `freshness_deadline` | TIMESTAMPTZ | | Internal |

#### `pgtrickle.pgt_change_tracking` — Per-source CDC slot tracking

| Column | Type | Constraints | API |
|--------|------|-------------|-----|
| `source_relid` | OID | PRIMARY KEY | Internal |
| `slot_name` | TEXT | NOT NULL | Internal |
| `last_consumed_lsn` | PG_LSN | | Internal |
| `tracked_by_pgt_ids` | BIGINT[] | | Internal |

### 2.3 Views

| View | Schema | API Level |
|------|--------|-----------|
| `pgtrickle.stream_tables_info` | `pgtrickle` | **Public** |
| `pgtrickle.pg_stat_stream_tables` | `pgtrickle` | **Public** |

### 2.4 SQL Functions (Public API)

| Function | Signature | Notes |
|----------|-----------|-------|
| `pgtrickle.create_stream_table` | `(name, query, schedule='1m', refresh_mode='DIFFERENTIAL', initialize=true)` | Core API |
| `pgtrickle.alter_stream_table` | `(name, schedule=NULL, refresh_mode=NULL, status=NULL)` | Core API |
| `pgtrickle.drop_stream_table` | `(name)` | Core API |
| `pgtrickle.refresh_stream_table` | `(name)` | Core API |
| `pgtrickle.pgt_status` | `()` → SETOF record | Status overview |
| `pgtrickle.parse_duration_seconds` | `(input)` → BIGINT | Utility |
| `pgtrickle.st_refresh_stats` | `()` → SETOF record | Monitoring |
| `pgtrickle.get_refresh_history` | `(name, max_rows=20)` → SETOF record | Monitoring |
| `pgtrickle.get_staleness` | `(name)` → FLOAT8 | Monitoring |
| `pgtrickle.slot_health` | `()` → SETOF record | Monitoring |
| `pgtrickle.explain_st` | `(name)` → SETOF record | Diagnostic |
| `pgtrickle.check_cdc_health` | `()` → SETOF record | Monitoring |

### 2.5 SQL Functions (Internal)

| Function | Purpose |
|----------|---------|
| `pgtrickle.pg_trickle_hash(text)` → BIGINT | Row identity hashing |
| `pgtrickle.pg_trickle_hash_multi(text[])` → BIGINT | Multi-column row hashing |
| `pgtrickle._on_ddl_end()` → event_trigger | DDL tracking |
| `pgtrickle._on_sql_drop()` → event_trigger | DROP tracking |

### 2.6 Event Triggers

| Name | Event | API Level |
|------|-------|-----------|
| `pg_trickle_ddl_tracker` | `ddl_command_end` | Internal |
| `pg_trickle_drop_tracker` | `sql_drop` | Internal |

### 2.7 Dynamic Objects (per stream table / source table)

| Object | Naming Pattern | Schema |
|--------|---------------|--------|
| Storage table | `<user_schema>.<user_name>` | User-specified |
| Change buffer table | `changes_<source_oid>` | `pgtrickle_changes` |
| CDC row trigger | `pg_trickle_cdc_<oid>` | On source table |
| CDC truncate trigger | `pg_trickle_cdc_truncate_<oid>` | On source table |
| CDC trigger function | `pg_trickle_cdc_fn_<oid>()` | `pgtrickle_changes` |
| CDC truncate function | `pg_trickle_cdc_truncate_fn_<oid>()` | `pgtrickle_changes` |
| Change buffer index | `idx_changes_<oid>_lsn_pk_cid` | `pgtrickle_changes` |
| Replication slot | `pgtrickle_<oid>` | Cluster-wide |
| Publication | `pgtrickle_cdc_<oid>` | Cluster-wide |

### 2.8 Hardcoded Column Names (Storage Tables)

| Column | Type | When Created | API Level |
|--------|------|-------------|-----------|
| `__pgt_row_id` | BIGINT | Always | **Public** (visible to users who query STs) |
| `__pgt_count` | BIGINT | Aggregate/DISTINCT queries | Internal (but visible) |

### 2.9 Hardcoded Column Names (Change Buffer Tables)

| Column | Type | API Level |
|--------|------|-----------|
| `change_id` | BIGSERIAL | Internal |
| `lsn` | PG_LSN | Internal |
| `action` | CHAR(1) | Internal |
| `pk_hash` | BIGINT | Internal |
| `new_<colname>` | Per-source | Internal |
| `old_<colname>` | Per-source | Internal |

### 2.10 GUC Variables (16 total)

| GUC | Type | Default | API Level |
|-----|------|---------|-----------|
| `pg_trickle.enabled` | BOOL | `true` | **Public** |
| `pg_trickle.scheduler_interval_ms` | INT | `1000` | Public |
| `pg_trickle.min_schedule_seconds` | INT | `60` | Public |
| `pg_trickle.max_consecutive_errors` | INT | `3` | Public |
| `pg_trickle.change_buffer_schema` | STRING | `'pgtrickle_changes'` | Public |
| `pg_trickle.max_concurrent_refreshes` | INT | `4` | Public |
| `pg_trickle.differential_max_change_ratio` | FLOAT | `0.15` | Public |
| `pg_trickle.cleanup_use_truncate` | BOOL | `true` | Public |
| `pg_trickle.merge_planner_hints` | BOOL | `true` | Public |
| `pg_trickle.merge_work_mem_mb` | INT | `64` | Public |
| `pg_trickle.merge_strategy` | STRING | `'auto'` | Public |
| `pg_trickle.use_prepared_statements` | BOOL | `true` | Public |
| `pg_trickle.user_triggers` | STRING | `'auto'` | Public |
| `pg_trickle.cdc_mode` | STRING | `'trigger'` | Public |
| `pg_trickle.wal_transition_timeout` | INT | `300` | Public |
| `pg_trickle.block_source_ddl` | BOOL | `false` | Public |

### 2.11 NOTIFY Channels

| Channel | API Level |
|---------|-----------|
| `pg_trickle_alert` | **Public** |
| `pg_trickle_cdc_transition` | Public |
| `pgtrickle_refresh` | Public |

### 2.12 Advisory Lock Keys

| Key | Purpose |
|-----|---------|
| `pgt_id` (BIGINT) | Concurrent refresh prevention |

---

## 3. Bugs and Inconsistencies to Fix Before 1.0

### 3.1 CHECK Constraint Bugs — CRITICAL

**Bug 1:** `pgt_stream_tables.refresh_mode` CHECK constraint lists
`'DIFFERENTIAL'` twice:

```sql
CHECK (refresh_mode IN ('FULL', 'DIFFERENTIAL', 'DIFFERENTIAL'))
```

Should be:

```sql
CHECK (refresh_mode IN ('FULL', 'DIFFERENTIAL'))
```

**Bug 2:** `pgt_refresh_history.action` CHECK constraint lists
`'DIFFERENTIAL'` twice:

```sql
CHECK (action IN ('NO_DATA', 'FULL', 'DIFFERENTIAL', 'DIFFERENTIAL', 'REINITIALIZE', 'SKIP'))
```

Should be:

```sql
CHECK (action IN ('NO_DATA', 'FULL', 'DIFFERENTIAL', 'REINITIALIZE', 'SKIP'))
```

**Severity:** Low (duplicates in CHECK are logically harmless but signal
sloppiness; they would become confusing in documentation or dump output).

**Fix:** Correct before 1.0 — straightforward edit in `src/lib.rs`.

### 3.2 Naming Inconsistency — GUC Prefix vs Schema

The GUC prefix is `pg_trickle.*` (with underscore), but the schema is
`pgtrickle` (no underscore). This is mildly confusing but changing either
now would be more disruptive than living with the inconsistency—the GUC
prefix is baked into `postgresql.conf` files and the schema into SQL scripts.

**Recommendation:** Document the convention explicitly. Do not change.

### 3.3 NOTIFY Channel Naming Inconsistency

Three channels use inconsistent naming:

- `pg_trickle_alert` — underscore style
- `pg_trickle_cdc_transition` — underscore style
- `pgtrickle_refresh` — no separator style

**Recommendation:** Rename `pgtrickle_refresh` → `pg_trickle_refresh` before
1.0 for consistency. This is a breaking change for any external listener, but
since we're pre-1.0, now is the time.

### 3.4 Missing Foreign Key on `pgt_refresh_history`

`pgt_refresh_history.pgt_id` references `pgt_stream_tables.pgt_id` logically
but lacks a formal FK constraint. When a stream table is dropped, orphan
history rows remain indefinitely.

**Recommendation:** Add `REFERENCES pgtrickle.pgt_stream_tables(pgt_id)
ON DELETE CASCADE` or implement periodic cleanup. The CASCADE approach is
simpler and appropriate since history is meaningless for dropped STs.

### 3.5 `pgt_refresh_history` Unbounded Growth

No built-in retention policy. Production deployments will accumulate millions
of rows in the refresh history table.

**Recommendation:** Add a GUC `pg_trickle.history_retention_days` (default: 30)
and a cleanup step in the scheduler that runs daily — `DELETE FROM
pgtrickle.pgt_refresh_history WHERE start_time < now() - interval '...'`.

### 3.6 `pgt_change_tracking` — Orphan Risk

The `tracked_by_pgt_ids BIGINT[]` column can contain stale pgt_id values
after a stream table is dropped if cleanup is interrupted.

**Recommendation:** Consider normalizing to a junction table, or accept
the array design and add explicit cleanup in `drop_stream_table_impl()`.
For 1.0, the current design is acceptable with a cleanup sweep.

---

## 4. Pre-1.0 Schema Changes to Improve Future Stability

### 4.1 Add a Schema Version Tracking Table

Create a metadata table to enable future migration scripts:

```sql
CREATE TABLE IF NOT EXISTS pgtrickle.pgt_schema_version (
    version     TEXT NOT NULL PRIMARY KEY,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    description TEXT
);
INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('1.0.0', 'Initial 1.0 release');
```

This lets migration scripts check the current schema version and apply
only the needed transformations.

### 4.2 Reserve Columns for Future Use

Adding columns to existing tables is backwards-compatible (new code can
handle NULL in new columns; old extension versions ignore unknown columns).
There is **no need to reserve columns**. Instead, follow the additive-only
principle:

- **Safe changes (minor version):** ADD COLUMN (nullable), ADD INDEX,
  CREATE VIEW, new GUC with default, new function
- **Unsafe changes (major version):** DROP COLUMN, ALTER COLUMN TYPE,
  RENAME COLUMN, drop/rename function, change function signature, rename GUC

### 4.3 Frontier JSONB Structure

The `frontier` column stores serialized JSON with this structure:

```json
{
  "sources": {
    "<oid>": {"lsn": "0/1A2B3C4", "snapshot_ts": "1708XXX"}
  },
  "data_timestamp": "1708XXXZ"
}
```

**Risk:** The structure uses OID strings as keys. OIDs can be reused after
table drop/recreate. The current code handles this correctly, but the
serialization format is internal to the Rust `Frontier` struct and could
change.

**Recommendation:** Treat the frontier JSONB column as fully opaque. Never
expose its structure in public API documentation. If the format needs to
change, a migration script can read/rewrite all frontier values.

### 4.4 Consider Extracting Schedule Parsing to SQL

`parse_duration_seconds()` is exposed as a public SQL function because the
views `stream_tables_info` and `pg_stat_stream_tables` depend on it. This
couples view definitions to a C function.

**Recommendation:** This is acceptable. The function is IMMUTABLE and
PARALLEL SAFE, making it safe for view inlining. Keep as-is.

### 4.5 Normalize `refresh_mode` and `status` into ENUMs

Currently, `refresh_mode`, `status`, `action`, `cdc_mode`, and `source_type`
are all TEXT with CHECK constraints. PostgreSQL ENUM types would provide type
safety but are notoriously hard to modify (ALTER TYPE ... ADD VALUE is
append-only; removing values requires recreating the type).

**Recommendation:** Keep TEXT with CHECK constraints. This is more flexible
for migrations — adding a new value just requires `ALTER TABLE ... DROP
CONSTRAINT ... ADD CONSTRAINT`. ENUMs would create migration headaches.

### 4.6 Add `pgt_id` FK to `pgt_refresh_history`

As noted in §3.4:

```sql
ALTER TABLE pgtrickle.pgt_refresh_history
    ADD CONSTRAINT fk_hist_pgt_id
    FOREIGN KEY (pgt_id) REFERENCES pgtrickle.pgt_stream_tables(pgt_id)
    ON DELETE CASCADE;
```

---

## 5. Public API Stability Contract

After 1.0, these surfaces are **stable** and subject to semantic versioning:

### 5.1 Tier 1 — Strong Stability (Breaking Change = Major Version)

| Surface | Contract |
|---------|----------|
| Function names & parameter order | `create_stream_table(name, query, ...)` |
| Function parameter defaults | `schedule DEFAULT '1m'` |
| Function return types | `pgt_status()` column names and types |
| View column names & types | `stream_tables_info.*`, `pg_stat_stream_tables.*` |
| GUC names | `pg_trickle.enabled`, etc. |
| Schema names | `pgtrickle`, `pgtrickle_changes` |
| Storage table visible columns | `__pgt_row_id` (always present), user columns |
| NOTIFY channel names | `pg_trickle_alert`, `pg_trickle_cdc_transition`, ... |

### 5.2 Tier 2 — Moderate Stability (Can Add, Cannot Remove)

| Surface | Contract |
|---------|----------|
| Catalog table columns | Can add nullable columns; cannot drop or rename |
| View columns | Can add columns at the end; cannot remove |
| Function output columns | Can append new columns to SETOF results |
| GUC values/ranges | Can add new values (e.g., new merge strategy); cannot remove |
| CHECK constraint values | Can add new enum-like values; cannot remove |

### 5.3 Tier 3 — Internal (Can Change with Migration Script)

| Surface | Contract |
|---------|----------|
| Catalog table internal columns | `frontier`, `functions_used`, `auto_threshold`, etc. |
| Change buffer table structure | Column types, index layout |
| Dynamic object naming conventions | `changes_<oid>`, `pg_trickle_cdc_<oid>` |
| Trigger function body (PL/pgSQL) | Generated code, may change per-version |
| Hash function implementation | `pg_trickle_hash` algorithm |
| Advisory lock key scheme | Using `pgt_id` as lock key |
| Frontier JSONB format | Internal serialization |
| Background worker name | `pg_trickle scheduler` |

---

## 6. Upgrade/Migration Strategy

### 6.1 PostgreSQL Extension Update Mechanism

PostgreSQL supports versioned extension upgrades via `ALTER EXTENSION`:

```sql
ALTER EXTENSION pg_trickle UPDATE TO '1.1.0';
```

This executes a migration script named `pg_trickle--1.0.0--1.1.0.sql`
installed alongside the extension shared library.

### 6.2 Migration Script Architecture

Following ADR-063's recommendation (Option 3: Hybrid), we use:

1. **pgrx auto-generated SQL** — for function signature updates (C functions
   are automatically re-registered with `CREATE OR REPLACE FUNCTION`)
2. **Hand-written migration scripts** — for catalog table changes, index
   additions, data migrations, view updates

Migration scripts live in `sql/` and follow the naming convention:

```
sql/pg_trickle--1.0.0--1.1.0.sql
sql/pg_trickle--1.1.0--1.2.0.sql
sql/pg_trickle--1.2.0--2.0.0.sql
```

### 6.3 Migration Script Template

```sql
-- pg_trickle--1.0.0--1.1.0.sql
-- Migration: 1.0.0 → 1.1.0

-- 1. Schema version check (fail-safe)
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pgtrickle.pgt_schema_version
        WHERE version = '1.0.0'
    ) THEN
        RAISE EXCEPTION 'pg_trickle: expected schema version 1.0.0, '
                         'run previous migrations first';
    END IF;
END $$;

-- 2. Catalog table changes (additive only for minor versions)
ALTER TABLE pgtrickle.pgt_stream_tables
    ADD COLUMN IF NOT EXISTS new_column TEXT;

-- 3. View updates (CREATE OR REPLACE preserves existing grants)
CREATE OR REPLACE VIEW pgtrickle.stream_tables_info AS
SELECT ... ;

-- 4. Function signature updates (handled by pgrx, typically no manual SQL)

-- 5. Data migrations
-- UPDATE pgtrickle.pgt_stream_tables SET new_column = ... WHERE ...;

-- 6. Index changes
-- CREATE INDEX IF NOT EXISTS ... ;

-- 7. Record schema version
INSERT INTO pgtrickle.pgt_schema_version (version, description)
VALUES ('1.1.0', 'Added new_column to pgt_stream_tables');
```

### 6.4 Dynamic Object Migration

Dynamic objects (change buffer tables, CDC triggers) present a unique
challenge: they are not managed by the extension DDL but created at
runtime by `create_stream_table`. Migration strategies:

**Option A: Reinitialize-on-upgrade** — Mark all STs as `needs_reinit = TRUE`
during migration. The scheduler will rebuild each ST on its next cycle,
recreating triggers and buffer tables with the new format. This is **safe
but slow** for large deployments.

**Option B: In-place migration** — The migration script iterates over active
STs and applies `ALTER TABLE` / `CREATE OR REPLACE FUNCTION` to each dynamic
object. This is **fast but complex** and must handle edge cases (missing
tables, in-progress refreshes, etc.).

**Option C: Lazy migration** — Dynamic objects are migrated on first access
(e.g., the refresh code checks the buffer table schema and adds missing
columns). This is transparent but adds code complexity.

**Recommendation:** Option A for major version upgrades. Option B for minor
version upgrades that only require additive changes to dynamic objects.
Include a `pgtrickle.migrate()` utility function that iterates over STs
and applies any needed dynamic object changes.

### 6.5 Breaking Changes Procedure (Major Version)

For unavoidable breaking changes (column removals, type changes, renamed
functions), the migration script must:

1. Create new structures alongside old ones
2. Copy/transform data from old to new
3. Drop old structures
4. Update schema version

Example: renaming `pgt_stream_tables.schedule` to `refresh_interval`:

```sql
-- Step 1: Add new column
ALTER TABLE pgtrickle.pgt_stream_tables ADD COLUMN refresh_interval TEXT;

-- Step 2: Copy data
UPDATE pgtrickle.pgt_stream_tables SET refresh_interval = schedule;

-- Step 3: Drop old column (after confirming all code uses new name)
ALTER TABLE pgtrickle.pgt_stream_tables DROP COLUMN schedule;
```

### 6.6 Rollback Strategy

PostgreSQL does not support downgrading extensions (`ALTER EXTENSION ... UPDATE
TO` only goes forward). For rollback:

1. **pg_dump before upgrade** — always take a logical backup
2. **Version-specific cleanup SQL** — ship a `pg_trickle--1.1.0--1.0.0.sql`
   rollback script for emergency use (best-effort)
3. **Test in staging** — always test upgrades against a production-like
   environment before applying

---

## 7. Specific Recommendations Before 1.0

### 7.1 Must-Fix (Blocking 1.0)

| # | Issue | Section | Effort |
|---|-------|---------|--------|
| 1 | Fix duplicate `'DIFFERENTIAL'` in CHECK constraints | §3.1 | 10 min |
| 2 | Add FK on `pgt_refresh_history.pgt_id` | §3.4 | 10 min |
| 3 | Add `pgtrickle.pgt_schema_version` table | §4.1 | 30 min |
| 4 | Rename `pgtrickle_refresh` → `pg_trickle_refresh` channel | §3.3 | 30 min |

### 7.2 Should-Fix (Recommended for 1.0)

| # | Issue | Section | Effort |
|---|-------|---------|--------|
| 5 | Add history retention GUC + scheduler cleanup | §3.5 | 2–3 hours |
| 6 | Document public API stability contract | §5 | 1 hour |
| 7 | Create migration script template | §6.3 | 1 hour |
| 8 | Validate orphan cleanup in `drop_stream_table` | §3.6 | 1 hour |

### 7.3 Nice-to-Have (Can Ship After 1.0)

| # | Issue | Section | Effort |
|---|-------|---------|--------|
| 9 | `pgtrickle.migrate()` utility function | §6.4 | 4–6 hours |
| 10 | Automated migration test infrastructure | §8 | 8–12 hours |

---

## 8. Testing Migration Scripts

### 8.1 Migration Test Strategy

Each migration script should be tested with:

1. **Fresh install test** — install version N directly; verify schema
2. **Upgrade test** — install version N-1, populate with test data,
   run `ALTER EXTENSION ... UPDATE TO 'N'`, verify all data preserved
3. **Idempotency test** — run migration twice; verify no errors
4. **Data integrity test** — verify FKs, indexes, and CHECK constraints
   still hold after migration

### 8.2 Test Infrastructure

Add an E2E test file `tests/e2e_migration_tests.rs` that:

1. Creates a container with pg_trickle version N-1 installed
2. Creates several stream tables with various configurations
3. Triggers refreshes to populate history
4. Upgrades to version N via `ALTER EXTENSION`
5. Verifies catalog integrity
6. Verifies stream tables still function (refresh, alter, drop)

---

## 9. Risk Assessment Summary

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Catalog column rename needed | Low | High | Additive-only policy (§4.2) |
| Function signature change | Medium | High | Careful parameter defaults (§5.1) |
| Frontier format change | Medium | Low | Opaque JSONB (§4.3), migration rewrite |
| Change buffer schema change | Medium | Low | Reinitialize-on-upgrade (§6.4 Option A) |
| GUC rename needed | Low | High | Document clearly, never rename (§5.1) |
| New CHECK constraint value | High | None | TEXT + CHECK is additive-safe (§4.5) |
| View column addition | High | None | Append-only, CREATE OR REPLACE |
| Hash function algorithm change | Low | High | Would require full reinit of all STs |
| Event trigger behavior change | Low | Medium | Transparent to users |

---

## 10. Comparison with Other PostgreSQL Extensions

| Extension | Schema versioning | Migration approach |
|-----------|------------------|--------------------|
| PostGIS | Version table | Hand-written upgrade scripts per version pair |
| pgaudit | No catalog tables | Configuration-only, no migration needed |
| pg_cron | Single metadata table | Manual ALTER TABLE in upgrade scripts |
| Citus | Version tracking | Elaborate migration framework with rollback |
| TimescaleDB | `_timescaledb_catalog.metadata` | Loader checks version, runs migrations |

pg_trickle should follow the PostGIS/TimescaleDB pattern: a schema version
table, hand-written migration scripts, and a pre-upgrade version check.

---

## 11. Conclusion

The pg_trickle database schema is in good shape for a 1.0 release after
addressing the four must-fix items (§7.1). The TEXT-with-CHECK-constraint
pattern for enum-like columns is the right choice for migration flexibility.
The separation between public-facing views/functions and internal catalog
columns provides a clean abstraction boundary.

The main risks are around **function signature stability** (adding parameters
with defaults is safe; changing return column sets requires view updates)
and **dynamic object migration** (change buffer tables and CDC triggers need
per-ST migration logic for structural changes).

The recommended approach is:

1. Fix the four must-fix items now
2. Ship 1.0 with clear documentation of the stability contract
3. Implement migration test infrastructure before 1.1
4. Follow additive-only changes for minor versions; save breaking changes
   for major versions with comprehensive migration scripts
