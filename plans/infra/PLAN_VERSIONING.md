# PLAN_VERSIONING.md — Semantic Versioning & Compatibility Policy

> **Status:** Draft  
> **Target version:** v1.0.0  
> **Author:** pg_stream project

---

## 1. Overview

pg_stream follows [Semantic Versioning 2.0.0](https://semver.org/) from v1.0.0
onward. Prior to 1.0 (0.x.y), minor-version bumps may include breaking changes
to the catalog schema or SQL API.

---

## 2. Version Number Rules

```
MAJOR.MINOR.PATCH
  │      │     └── Backwards-compatible bug fixes only
  │      └──────── New SQL functions / GUCs / catalog columns (non-breaking)
  └─────────────── Incompatible catalog schema changes or SQL API removals
```

### What constitutes a MAJOR (breaking) change

| Change | Breaking? |
|--------|-----------|
| Rename / drop a SQL function in schema `pgstream` | Yes |
| Remove or rename a column in `pgstream.pgs_stream_tables` | Yes |
| Change a GUC name | Yes |
| Change the default behavior of an existing function | Yes |
| Require a new `ALTER EXTENSION UPDATE` migration | Yes |
| Add a new optional GUC | No |
| Add a new monitoring view | No |
| Add a new SQL function | No |
| Add a nullable/defaulted column to a catalog table | No |

### Pre-1.0 policy (0.x.y)

- `0.MINOR.0` bumps MAY break catalog schema.
- `0.x.PATCH` bumps MUST NOT break the catalog schema.
- All breaking changes MUST be documented in [CHANGELOG.md](../../CHANGELOG.md).

---

## 3. PostgreSQL Extension Upgrade Scripts

### 3.1 File naming

Upgrade scripts live in the repository root (alongside `pg_stream.control`):

```
pg_stream--0.1.0.sql          # Initial install script
pg_stream--0.1.0--0.2.0.sql  # Upgrade path 0.1.0 → 0.2.0
pg_stream--0.2.0--0.3.0.sql  # Upgrade path 0.2.0 → 0.3.0
```

Multi-hop upgrades are supported by PostgreSQL automatically (it chains
individual step scripts), but we SHOULD also provide direct paths for common
jumps (e.g., `0.1.0--1.0.0.sql`) to reduce downtime.

### 3.2 `pg_stream.control` fields

```ini
default_version = '0.1.0'     # Updated on every release
module_pathname = '$libdir/pg_stream'
relocatable = false
schema = pgstream
requires = ''
```

`default_version` MUST be bumped as part of every release PR before tagging.

### 3.3 Running an upgrade

```sql
ALTER EXTENSION pg_stream UPDATE;                -- to latest
ALTER EXTENSION pg_stream UPDATE TO '0.2.0';    -- to specific version
SELECT extversion FROM pg_extension WHERE extname = 'pg_stream';
```

Detailed migration SQL authoring guidelines: see
[PLAN_UPGRADE_MIGRATIONS.md](../sql/PLAN_UPGRADE_MIGRATIONS.md).

---

## 4. Public API Definition

The following surface area is considered **public** and governed by semver:

| Surface | Location |
|---------|----------|
| SQL functions | All `CREATE FUNCTION` in schema `pgstream` |
| Catalog table columns | `pgstream.pgs_stream_tables.*` |
| GUC names | `pg_stream.*` parameters in `postgresql.conf` |
| Change buffer schema | `pgstream_changes.changes_<oid>` column names |
| SQL error codes | Any `SQLSTATE` codes documented in SQL_REFERENCE.md |

The following are **internal** and NOT subject to semver:

- Function names prefixed with `_pgstream_` (internal helpers)
- Trigger function names (`_pgstream_cdc_trigger`, etc.)
- Shared memory layout
- Background worker names

---

## 5. Deprecation Policy

1. Functions/GUCs marked deprecated remain available for **one full MINOR
   version** before removal.
2. A deprecation notice appears in the SQL function comment and in
   [CHANGELOG.md](../../CHANGELOG.md).
3. A `WARNING`-level notice is emitted at call time:
   ```sql
   RAISE WARNING 'pgstream.foo() is deprecated and will be removed in v2.0. Use pgstream.bar() instead.';
   ```
4. The deprecated item is removed in the next MAJOR bump.

---

## 6. Compatibility Matrix

| pg_stream | PostgreSQL | pgrx  | Notes |
|-----------|-----------|-------|-------|
| 0.1.x | 18.x | 0.17.x | Pre-release |
| 0.2.x | 18.x | 0.17.x | |
| 0.3.x | 18.x | 0.17.x | |
| 1.0.x | 18.x | 0.17.x | First stable |
| 1.1.x | 18.x, 19.x | 0.18+ | See [PLAN_PG19_COMPAT.md](PLAN_PG19_COMPAT.md) |

---

## 7. Release Checklist

- [ ] Bump `default_version` in `pg_stream.control`
- [ ] Bump `version` in `Cargo.toml`
- [ ] Write upgrade SQL script for all supported upgrade paths
- [ ] Add `## vX.Y.Z` section to `CHANGELOG.md`
- [ ] Tag commit `vX.Y.Z` after CI passes
- [ ] Push tag to trigger GitHub Actions packaging workflow

---

## References

- [CHANGELOG.md](../../CHANGELOG.md)
- [pg_stream.control](../../pg_stream.control)
- [Cargo.toml](../../Cargo.toml)
- [PLAN_UPGRADE_MIGRATIONS.md](../sql/PLAN_UPGRADE_MIGRATIONS.md)
- [PLAN_PG19_COMPAT.md](PLAN_PG19_COMPAT.md)
