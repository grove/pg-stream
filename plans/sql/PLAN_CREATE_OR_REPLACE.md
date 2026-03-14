# Plan: `create_or_replace_stream_table`

**Status:** Complete (Steps 1–7 done)  
**Author:** Copilot  
**Date:** 2026-03-05  
**Depends on:** [PLAN_ALTER_QUERY.md](PLAN_ALTER_QUERY.md)

---

## 1. Motivation

Deploying stream table definitions is non-idempotent today. Users must check
existence and manually orchestrate drop+create or alter. This is friction for
every deployment pattern:

| Deployment pattern | Current workaround | Problem |
|---|---|---|
| **SQL migrations** | `SELECT pgtrickle.drop_stream_table(...)` then `SELECT pgtrickle.create_stream_table(...)` | Data gap; lost metadata (schedule, diamond settings, refresh history) |
| **dbt** | Materialization macro checks catalog, compares query, decides drop+create vs. alter | Complex macro logic; still has data gap on query change |
| **GitOps / IaC** | Hand-rolled existence checks in Terraform/Pulumi providers | Fragile; not atomic |
| **Interactive development** | Developer must remember current state, pick the right call | Error-prone; `AlreadyExists` if you forget |

PostgreSQL has `CREATE OR REPLACE VIEW`, `CREATE OR REPLACE FUNCTION`, and
(as of PG 16) `CREATE OR REPLACE TRIGGER`. Stream tables should follow the
same convention.

### Design principle

The user declares intent: "I want this stream table to exist with this
definition." The system figures out the delta. This is the **declarative**
model that modern database workflows expect.

---

## 2. Proposed API

```sql
pgtrickle.create_or_replace_stream_table(
    name                    text,
    query                   text,
    schedule                text      DEFAULT 'calculated',
    refresh_mode            text      DEFAULT 'DIFFERENTIAL',
    initialize              bool      DEFAULT true,
    diamond_consistency     text      DEFAULT NULL,
    diamond_schedule_policy text      DEFAULT NULL
) → void
```

Same signature as `create_stream_table`. The name mirrors PostgreSQL's
`CREATE OR REPLACE` convention.

### Semantics

| Current state | Action taken |
|---|---|
| Stream table does **not** exist | **Create** — identical to `create_stream_table(...)` |
| Stream table exists, query **and** all config identical | **No-op** — log INFO and return |
| Stream table exists, query identical but config differs | **Alter config** — delegates to `alter_stream_table(...)` for schedule, refresh_mode, diamond settings |
| Stream table exists, query differs | **Replace query** — delegates to the ALTER QUERY path from [PLAN_ALTER_QUERY.md](PLAN_ALTER_QUERY.md), plus config changes |

The `initialize` parameter is honoured on **create** only. On replace, the
stream table is always repopulated via a full refresh (the ALTER QUERY path
handles this in Phase 5). Passing `initialize => false` when the stream table
already exists is silently ignored — the table already has data and the new
query must be materialized.

---

## 3. Decision matrix: what constitutes "identical"

### Query comparison

The **rewritten** (post-pipeline) defining query is compared, not the raw
user input. This ensures cosmetic SQL differences (whitespace, casing,
extra parentheses) and view definition changes are handled correctly:

| User input change | Rewritten query | Result |
|---|---|---|
| Whitespace / casing only | Same | No-op |
| Added a comment | Same (comments stripped by parser) | No-op |
| View definition changed upstream | Different (after inlining) | Replace |
| Query logic changed | Different | Replace |

Implementation: normalize both queries through the rewrite pipeline, then
compare the resulting strings. If string comparison is too brittle (ordering
of implicit casts, etc.), fall back to a content-hash comparison of the
`LIMIT 0` column metadata + the raw rewritten SQL.

### Config comparison

| Parameter | Comparison |
|---|---|
| `schedule` | String equality after normalization (`'1m'` vs `'60s'` are different — we compare the literal, same as today) |
| `refresh_mode` | Case-insensitive enum match |
| `diamond_consistency` | Case-insensitive enum match |
| `diamond_schedule_policy` | Case-insensitive enum match |

---

## 4. Interaction with existing functions

### Relationship to `create_stream_table`

`create_stream_table` remains unchanged — it errors on duplicate names. This
is the "strict" API for users who want an explicit error if they accidentally
create the same stream table twice (analogous to `CREATE VIEW` vs.
`CREATE OR REPLACE VIEW`).

### Relationship to `alter_stream_table`

`create_or_replace` delegates to `alter_stream_table_impl` for config changes
and to the ALTER QUERY path for query changes. It does not duplicate logic.

### Relationship to `drop_stream_table`

`create_or_replace` never drops a stream table. Even when the query changes
and the schema is incompatible, it uses the ALTER QUERY path's "full rebuild"
strategy (DROP TABLE + CREATE TABLE on the **storage** table, preserving the
catalog entry and `pgt_id`).

---

## 5. Additional convenience: `IF NOT EXISTS` variant

As a complementary feature, add:

```sql
pgtrickle.create_stream_table_if_not_exists(
    name                    text,
    query                   text,
    schedule                text      DEFAULT 'calculated',
    refresh_mode            text      DEFAULT 'DIFFERENTIAL',
    initialize              bool      DEFAULT true,
    diamond_consistency     text      DEFAULT NULL,
    diamond_schedule_policy text      DEFAULT NULL
) → void
```

| Current state | Action |
|---|---|
| Does not exist | Create (same as `create_stream_table`) |
| Exists | No-op (log INFO, return) |

This is useful for migration scripts that should be safe to re-run but should
**not** silently change an existing stream table's definition.

---

## 6. Implementation

### Step 1: Core `create_or_replace_stream_table` function
**File:** `src/api.rs`  
**Effort:** ~3 hours

```rust
#[pg_extern(schema = "pgtrickle")]
fn create_or_replace_stream_table(
    name: &str,
    query: &str,
    schedule: default!(Option<&str>, "'calculated'"),
    refresh_mode: default!(&str, "'DIFFERENTIAL'"),
    initialize: default!(bool, true),
    diamond_consistency: default!(Option<&str>, "NULL"),
    diamond_schedule_policy: default!(Option<&str>, "NULL"),
) {
    let result = create_or_replace_stream_table_impl(
        name, query, schedule, refresh_mode, initialize,
        diamond_consistency, diamond_schedule_policy,
    );
    if let Err(e) = result {
        pgrx::error!("{}", e);
    }
}
```

Implementation logic:

```rust
fn create_or_replace_stream_table_impl(
    name: &str,
    query: &str,
    schedule: Option<&str>,
    refresh_mode_str: &str,
    initialize: bool,
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
) -> Result<(), PgTrickleError> {
    let (schema, table_name) = parse_qualified_name(name)?;

    match StreamTableMeta::get_by_name(&schema, &table_name) {
        Ok(existing) => {
            // Stream table exists — determine what changed
            let new_query_rewritten = run_query_rewrite_pipeline(query)?;
            let query_changed = existing.defining_query != new_query_rewritten;

            let config_changes = compute_config_diff(
                &existing, schedule, refresh_mode_str,
                diamond_consistency, diamond_schedule_policy,
            );

            if !query_changed && config_changes.is_empty() {
                pgrx::info!(
                    "Stream table {}.{} already exists with identical definition — no changes made.",
                    schema, table_name,
                );
                return Ok(());
            }

            if query_changed {
                // Delegate to ALTER QUERY path (from PLAN_ALTER_QUERY)
                // which handles schema migration, CDC teardown/setup,
                // and full refresh.
                alter_stream_table_impl(
                    name,
                    Some(query),             // new query
                    config_changes.schedule,
                    config_changes.refresh_mode,
                    None,                    // status: keep current
                    config_changes.diamond_consistency,
                    config_changes.diamond_schedule_policy,
                )?;
            } else {
                // Only config changed — lightweight alter
                alter_stream_table_impl(
                    name,
                    None,                    // no query change
                    config_changes.schedule,
                    config_changes.refresh_mode,
                    None,
                    config_changes.diamond_consistency,
                    config_changes.diamond_schedule_policy,
                )?;
            }

            Ok(())
        }
        Err(PgTrickleError::NotFound(_)) => {
            // Does not exist — create
            create_stream_table_impl(
                name, query, schedule, refresh_mode_str,
                initialize, diamond_consistency, diamond_schedule_policy,
            )
        }
        Err(e) => Err(e),
    }
}
```

### Step 2: `create_stream_table_if_not_exists`
**File:** `src/api.rs`  
**Effort:** ~30 min

Trivial wrapper: try `get_by_name`, if found log INFO and return Ok,
if not found delegate to `create_stream_table_impl`.

### Step 3: Config diff utility
**File:** `src/api.rs`  
**Effort:** ~1 hour

```rust
struct ConfigDiff {
    schedule: Option<&str>,         // Some only if changed
    refresh_mode: Option<&str>,     // Some only if changed
    diamond_consistency: Option<&str>,
    diamond_schedule_policy: Option<&str>,
}

fn compute_config_diff(
    existing: &StreamTableMeta,
    new_schedule: Option<&str>,
    new_refresh_mode: &str,
    new_dc: Option<&str>,
    new_dsp: Option<&str>,
) -> ConfigDiff
```

Compares each parameter against the existing catalog row. Returns `None` for
unchanged parameters (which `alter_stream_table_impl` interprets as "keep
current").

### Step 4: Upgrade SQL migration
**File:** `sql/pg_trickle--<prev>--<next>.sql`  
**Effort:** ~30 min

```sql
-- New functions added by pgrx #[pg_extern]; no manual SQL needed if
-- we are on the same extension version. For upgrades, the migration script
-- must register the new function signatures.
```

### Step 5: Update dbt materialization
**File:** `dbt-pgtrickle/macros/materializations/stream_table.sql`  
**Effort:** ~2 hours

Replace the current check-exists → compare-query → drop+create / alter logic
with a single call:

```sql
SELECT pgtrickle.create_or_replace_stream_table(
    name         => 'schema.table',
    query        => '...',
    schedule     => '1m',
    refresh_mode => 'DIFFERENTIAL'
);
```

The entire `{% if not st_exists %}` / `{% else %}` / `{% if query changed %}`
block collapses to one function call. The dbt materialization becomes
dramatically simpler.

Keep backward compatibility: if the function doesn't exist (older pgtrickle
version), fall back to the current drop+create pattern.

### Step 6: Documentation
**Files:** `docs/SQL_REFERENCE.md`, `docs/FAQ.md`, `docs/GETTING_STARTED.md`  
**Effort:** ~2 hours

- Add `create_or_replace_stream_table` and `create_stream_table_if_not_exists`
  to the SQL reference.
- Update the "How do I change a stream table's query?" FAQ answer.
- Add deployment best-practices section recommending `create_or_replace` for
  migration scripts.

### Step 7: Tests
**File:** `tests/e2e_create_or_replace_tests.rs` (new)  
**Effort:** ~4 hours

| Test | Scenario |
|---|---|
| `test_cor_creates_when_not_exists` | Stream table doesn't exist → created |
| `test_cor_noop_when_identical` | Same query + config → no-op, no full refresh |
| `test_cor_alters_config_only` | Same query, different schedule → alter |
| `test_cor_replaces_query_same_schema` | Different query, same output columns → in-place replace |
| `test_cor_replaces_query_new_columns` | Different query, added columns → storage migration |
| `test_cor_replaces_query_incompatible` | Column type change → full rebuild |
| `test_cor_replaces_query_and_config` | Both query and schedule changed → both applied |
| `test_cor_immediate_mode` | Create-or-replace with IMMEDIATE mode |
| `test_cor_differential_to_full` | Existing DIFFERENTIAL, replace with FULL mode + new query |
| `test_cor_concurrent_readers` | Readers see old data during replace, new data after |
| `test_if_not_exists_creates` | Doesn't exist → created |
| `test_if_not_exists_noop` | Exists → no-op regardless of query/config differences |
| `test_cor_dbt_integration` | dbt materialization uses create_or_replace |

---

## 7. Sequencing & Dependencies

```
PLAN_ALTER_QUERY (Step 1: refactor)
        │
        ├──► PLAN_ALTER_QUERY (Steps 2-5: core ALTER QUERY)
        │           │
        │           ▼
        │    PLAN_CREATE_OR_REPLACE (Steps 1-3: core function)
        │           │
        │           ├──► Step 4: upgrade SQL
        │           ├──► Step 5: dbt materialization
        │           ├──► Step 6: docs
        │           └──► Step 7: tests
        │
        └──► PLAN_CREATE_OR_REPLACE (Step 2: IF NOT EXISTS)
```

`create_or_replace` depends on the ALTER QUERY implementation from
[PLAN_ALTER_QUERY.md](PLAN_ALTER_QUERY.md) being complete first. The
`if_not_exists` variant has no dependency on ALTER QUERY and can be
implemented immediately.

---

## 8. Alternatives Considered

### A. Single `upsert_stream_table` function

Combine create, alter, and replace into one function named `upsert`.

**Rejected:** "Upsert" is a DML concept. `CREATE OR REPLACE` is the
established PostgreSQL DDL convention and instantly communicates intent
to any PostgreSQL developer.

### B. `replace_stream_table` (always drop + create)

A function that always drops and recreates, without the smart diffing.

**Rejected:** Loses all advantages of in-place ALTER (preserved OID,
no data gap, retained metadata). If users want this behavior they can
call `drop_stream_table` + `create_stream_table` explicitly.

### C. Overload `create_stream_table` with an `or_replace` boolean

```sql
pgtrickle.create_stream_table(
    name text, query text, ...,
    or_replace bool DEFAULT false
)
```

**Rejected:** Deviates from PostgreSQL conventions. `CREATE OR REPLACE`
is always a distinct statement/function, not a flag. A boolean buried
in the parameter list is easy to miss and harder to search for in
migration scripts.

### D. Wait for native PostgreSQL syntax (`CREATE OR REPLACE STREAM TABLE`)

Defer to the native syntax plan ([PLAN_NATIVE_SYNTAX.md](PLAN_NATIVE_SYNTAX.md))
which might include `CREATE OR REPLACE` as part of a DDL grammar extension.

**Rejected for now:** Native syntax is a much larger undertaking
(requires a PostgreSQL parser hook or event trigger). The function-based
API can ship immediately and the native syntax can delegate to it later.

---

## 9. Risks & Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| ALTER QUERY plan not yet implemented | Blocks query-change path | `if_not_exists` can ship independently; `create_or_replace` with query change returns an actionable error until ALTER QUERY lands |
| Query normalization mismatches (false "changed") | Unnecessary full refresh | Compare rewritten SQL + column metadata hash; document that cosmetic differences may trigger a refresh |
| User expects `create_or_replace` to preserve data on schema change | Surprise data loss on incompatible schema | Emit WARNING for incompatible schema; document clearly |
| dbt backward compatibility | Old pgtrickle versions don't have the function | dbt macro checks function existence; falls back to current pattern |

---

## 10. Effort Summary

| Step | Effort |
|---|---|
| Step 1: Core `create_or_replace` function | 3h |
| Step 2: `if_not_exists` function | 0.5h |
| Step 3: Config diff utility | 1h |
| Step 4: Upgrade SQL | 0.5h |
| Step 5: dbt materialization update | 2h |
| Step 6: Documentation | 2h |
| Step 7: Tests | 4h |
| **Total** | **~13h** |

(Excludes the ~27h for [PLAN_ALTER_QUERY.md](PLAN_ALTER_QUERY.md) which is a
prerequisite for the query-change path but not for `if_not_exists` or
config-only changes.)
