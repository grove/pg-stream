# PLAN_UPGRADE_MIGRATIONS.md — Extension Upgrade Migrations

> **Status:** Draft  
> **Target version:** v0.3.0  
> **Related gap:** SQL_GAPS_7.md G8.2  
> **Author:** pg_trickle project

---

## 1. Overview

pg_trickle embeds catalog tables (`pgtrickle.pgt_stream_tables`) and change
buffer tables (`pgtrickle_changes.changes_<oid>`) inside the user's database.
As the extension evolves, these schemas must be migrated safely when a user
runs `ALTER EXTENSION pg_trickle UPDATE`.

This document defines the migration authoring and testing strategy.

---

## 2. How PostgreSQL Extension Upgrades Work

```sql
-- User runs:
ALTER EXTENSION pg_trickle UPDATE;
-- PostgreSQL resolves the chain:
--   default_version in pg_trickle.control → say '0.3.0'
--   installed version → say '0.1.0'
-- PostgreSQL executes in order:
--   pg_trickle--0.1.0--0.2.0.sql
--   pg_trickle--0.2.0--0.3.0.sql
```

Each upgrade script MUST be idempotent (safe to run on an already-upgraded
database) and MUST NOT DROP anything that a user might depend on without a
deprecation cycle.

---

## 3. Expected Schema Changes per Milestone

### v0.1.0 → v0.2.0

`pgtrickle.pgt_stream_tables` additions expected:

| Column | Type | Default | Reason |
|--------|------|---------|--------|
| `cdc_mode` | `text` | `'trigger'` | Explicit CDC mode storage |
| `last_error` | `text` | `NULL` | Last refresh error for monitoring |
| `consecutive_failures` | `int` | `0` | Retry state |

Upgrade script template:

```sql
-- pg_trickle--0.1.0--0.2.0.sql

-- Add new catalog columns if not already present
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM information_schema.columns
     WHERE table_schema = 'pgtrickle'
       AND table_name   = 'pgt_stream_tables'
       AND column_name  = 'cdc_mode'
  ) THEN
    ALTER TABLE pgtrickle.pgt_stream_tables
      ADD COLUMN cdc_mode text NOT NULL DEFAULT 'trigger';
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM information_schema.columns
     WHERE table_schema = 'pgtrickle'
       AND table_name   = 'pgt_stream_tables'
       AND column_name  = 'last_error'
  ) THEN
    ALTER TABLE pgtrickle.pgt_stream_tables
      ADD COLUMN last_error text;
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM information_schema.columns
     WHERE table_schema = 'pgtrickle'
       AND table_name   = 'pgt_stream_tables'
       AND column_name  = 'consecutive_failures'
  ) THEN
    ALTER TABLE pgtrickle.pgt_stream_tables
      ADD COLUMN consecutive_failures int NOT NULL DEFAULT 0;
  END IF;
END
$$;
```

### v0.2.0 → v0.3.0

TBD — expected to add parallelism-related columns (DAG level cache, worker
assignment). Script will follow the same `DO $$ IF NOT EXISTS` pattern.

---

## 4. Change Buffer Handling During Upgrade

`pgtrickle_changes.changes_<oid>` tables are created dynamically per source
table. They contain in-flight change records that should NOT be discarded
during an upgrade.

**Strategy:**

1. The upgrade script does NOT touch `pgtrickle_changes.*` tables directly.
2. If the change buffer schema changes (new columns), the Rust code handles
   missing columns via `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` during the
   first CDC trigger fire after upgrade.
3. The upgrade script bumps a `schema_version` column in
   `pgtrickle.pgt_stream_tables` so the Rust code knows which migration it is
   on. (Schema to be added in v0.2.0.)

**In-flight record safety:**

- Upgrade is performed inside a transaction if possible.
- The background worker's scheduler loop checks `pg_trickle.enabled` and will
  pause processing during DDL transactions (lock contention will cause the
  worker tick to skip).
- Recommendation: perform upgrades during low-traffic windows.

---

## 5. Rollback / Downgrade

PostgreSQL does not support automatic extension downgrades. If a downgrade is
needed:

1. `DROP EXTENSION pg_trickle CASCADE;` — **destroys all streaming tables**
2. Install old version: `CREATE EXTENSION pg_trickle VERSION '0.1.0';`
3. Re-create streaming tables from source definitions.

This is destructive. The v1.0.0 release should include a `pg_trickle_config`
dump tool that exports all streaming table definitions to a SQL script,
enabling recreation after a rollback.

---

## 6. Upgrade Path Testing in CI

```rust
// tests/e2e_upgrade_tests.rs  (to be written)
// Test: install 0.1.0, populate data, run ALTER EXTENSION UPDATE to 0.2.0,
//       verify catalog columns exist, verify stream tables still refresh.
#[tokio::test]
async fn test_upgrade_0_1_0_to_0_2_0_catalog_columns() { ... }

#[tokio::test]
async fn test_upgrade_0_1_0_to_0_2_0_existing_stream_tables_survive() { ... }
```

The E2E Docker image (`tests/Dockerfile.e2e`) SHOULD be parameterized by
`PG_STREAM_FROM_VERSION` so we can install the old version from a pre-built
`.so`, then upgrade.

---

## 7. Authoring Checklist

- [ ] Write `pg_trickle--X--Y.sql` in repo root
- [ ] Use `IF NOT EXISTS` guards for all `ALTER TABLE ADD COLUMN`
- [ ] Test by installing X, running the script manually, verifying output
- [ ] Add E2E upgrade test in `tests/e2e_upgrade_tests.rs`
- [ ] Bump `default_version` in `pg_trickle.control`
- [ ] Document changes in `CHANGELOG.md`

---

## References

- [plans/sql/SQL_GAPS_7.md](SQL_GAPS_7.md) — G8.2
- [src/catalog.rs](../../src/catalog.rs)
- [PLAN_VERSIONING.md](../infra/PLAN_VERSIONING.md)
- [PostgreSQL Extension Versioning](https://www.postgresql.org/docs/current/extend-extensions.html)
