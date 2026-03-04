# Plan: Allow `alter_stream_table` to Change the Defining Query

**Status:** Draft  
**Author:** Copilot  
**Date:** 2026-03-04

---

## 1. Motivation

Today, changing a stream table's defining query requires a **drop + recreate**
cycle:

```sql
SELECT pgtrickle.drop_stream_table('order_totals');
SELECT pgtrickle.create_stream_table('order_totals', 'SELECT ...', '1m', 'DIFFERENTIAL');
```

This has three user-facing problems:

1. **Data gap.** Between drop and recreate, the stream table does not exist.
   Concurrent queries against it fail with "relation does not exist."
2. **Lost metadata.** Schedule, refresh mode, diamond settings, and refresh
   history are discarded. The user must re-specify them.
3. **dbt friction.** The dbt materialization detects query changes and does a
   full drop/recreate, producing the same data gap and a `--full-refresh`
   equivalent even for minor query tweaks.

Adding a `query` parameter to `alter_stream_table` would allow in-place
query evolution with no downtime and minimal data disruption.

---

## 2. Current State

### What `alter_stream_table` can change today

| Parameter | Behavior |
|---|---|
| `schedule` | UPDATE catalog row |
| `refresh_mode` | Migrate trigger infrastructure (CDC ↔ IVM), UPDATE catalog |
| `status` | UPDATE catalog row, reset consecutive_errors |
| `diamond_consistency` | UPDATE catalog row |
| `diamond_schedule_policy` | UPDATE catalog row |

### What `create_stream_table` does that ALTER would need to replicate

1. **Query rewrite pipeline** — view inlining, DISTINCT ON, GROUPING SETS,
   scalar subquery, SubLinks-in-OR rewrites.
2. **Query validation** — `LIMIT 0` execution, unsupported construct checks,
   DVM parsing (for DIFFERENTIAL/IMMEDIATE), volatility checks.
3. **TopK detection** — extract ORDER BY + LIMIT pattern.
4. **Storage table creation** — `CREATE TABLE` with `__pgt_row_id`, optional
   `__pgt_count` / `__pgt_count_l` / `__pgt_count_r` columns, indexes.
5. **Catalog insert** — `pgt_stream_tables` row, `pgt_dependencies` rows with
   column snapshots.
6. **CDC/IVM infrastructure** — triggers + change buffer tables on source
   tables.
7. **DAG signal** — notify scheduler to rebuild dependency graph.
8. **Cache invalidation** — bump `CACHE_GENERATION` to flush delta/MERGE
   template caches.
9. **Initial full refresh** — populate the storage table.

### The `needs_reinit` (reinitialize) path

Today, `execute_reinitialize_refresh` is just `execute_full_refresh` +
clearing the `needs_reinit` flag. It does **not** drop/recreate the storage
table or update the column schema. This means the current reinitialize only
works when the output schema is unchanged (e.g., a function redefinition
that changes behavior but not column types).

---

## 3. Design

### 3.1 New parameter

```sql
pgtrickle.alter_stream_table(
    name                    text,
    query                   text      DEFAULT NULL,   -- NEW
    schedule                text      DEFAULT NULL,
    refresh_mode            text      DEFAULT NULL,
    status                  text      DEFAULT NULL,
    diamond_consistency     text      DEFAULT NULL,
    diamond_schedule_policy text      DEFAULT NULL
) → void
```

When `query` is non-NULL, the function performs a **query migration** in
addition to any other parameter changes.

### 3.2 Schema classification

The first step after validating the new query is to classify the schema change
by comparing old vs. new output columns:

| Classification | Condition | Strategy |
|---|---|---|
| **Same schema** | Column names, types, and count are identical | Fast path: update catalog + full refresh |
| **Compatible schema** | Columns added, removed, or reordered; no type conflicts on surviving columns | Storage migration: `ALTER TABLE` the storage table + full refresh |
| **Incompatible schema** | Column type changed in a way that's not implicitly castable | Full rebuild: drop + recreate storage table |

### 3.3 Execution phases

#### Phase 0 — Validate & classify

1. Run the **full rewrite pipeline** on the new query (view inlining, DISTINCT
   ON, GROUPING SETS, etc.) — same code path as `create_stream_table_impl`.
2. Run **all validation checks** (LIMIT 0, unsupported constructs, DVM parse
   for DIFFERENTIAL/IMMEDIATE, volatility, TopK detection).
3. Extract new output columns via `validate_defining_query()`.
4. Compare against current storage table columns to determine schema
   classification.
5. Extract new source dependencies via `extract_source_relations()`.
6. Run **cycle detection** on the new dependency set.

If any validation fails, the ALTER is rejected and the existing stream table
is untouched (transactional safety).

#### Phase 1 — Suspend & drain

1. Set `status = 'SUSPENDED'` to prevent the scheduler from refreshing during
   migration.
2. Flush any pending deferred cleanup for this ST's source OIDs.

#### Phase 2 — Tear down old infrastructure

1. **Diff old vs. new source dependencies.**
   - Sources **removed**: drop CDC triggers + change buffer tables (or IVM
     triggers for IMMEDIATE mode).
   - Sources **kept**: leave CDC infra in place; update column snapshots if
     needed.
   - Sources **added**: will be set up in Phase 4.
2. Invalidate MERGE template cache (`bump_cache_generation()`).
3. Flush prepared-statement tracking for this `pgt_id`.

#### Phase 3 — Migrate storage table

Depends on schema classification:

**Same schema (fast path):**
- No DDL required. Storage table is reused as-is.

**Compatible schema:**
- `ALTER TABLE ... ADD COLUMN` for new columns.
- `ALTER TABLE ... DROP COLUMN` for removed columns.
- Update or recreate the `__pgt_row_id` unique index.
- Recreate any GROUP BY composite index if the group-by columns changed.
- Handle `__pgt_count` / `__pgt_count_l` / `__pgt_count_r` auxiliary columns:
  add or drop as needed.

**Incompatible schema (full rebuild):**
- `DROP TABLE ... CASCADE` the existing storage table.
- `CREATE TABLE` with new schema (same code as `create_stream_table_impl`).
- The stream table OID changes — update `pgt_relid` in the catalog.

#### Phase 4 — Update catalog & set up new infrastructure

1. **Update `pgt_stream_tables`:**
   - `defining_query` ← new rewritten query
   - `original_query` ← new user-supplied query (if view inlining applies)
   - `functions_used` ← re-extracted from new DVM parse
   - `topk_limit` / `topk_order_by` / `topk_offset` ← from new TopK
     detection
   - Clear `needs_reinit` flag
   - Reset `frontier` to NULL (force full refresh)
   - Reset `is_populated` to FALSE
   - Update `updated_at`
2. **Update `pgt_dependencies`:**
   - Delete old dependency rows.
   - Insert new dependency rows with column snapshots/fingerprints.
3. **Set up CDC/IVM for new sources:**
   - New source tables get CDC triggers + change buffer tables (or IVM
     triggers for IMMEDIATE mode).
   - Same logic as `create_stream_table_impl` Phase 2.
4. **Signal DAG rebuild** (`signal_dag_rebuild()`).
5. **Bump cache generation** (`bump_cache_generation()`).

#### Phase 5 — Repopulate

1. Execute a **full refresh** to populate the storage table with the new
   query's results.
2. Compute and store the initial frontier.
3. Set `status = 'ACTIVE'` and `is_populated = TRUE`.

All of Phases 1–5 execute within a **single SPI transaction**, so the
stream table atomically transitions from old query → new query. Concurrent
readers see either the old data (before commit) or the new data (after commit),
never an empty table.

### 3.4 Transactional safety

Unlike drop + recreate, the ALTER approach keeps the storage table's OID
stable (for same-schema and compatible-schema cases). This means:

- Views and policies referencing the stream table remain valid.
- Logical replication publications continue working.
- `pg_depend` entries are preserved.
- No "relation does not exist" window for concurrent queries.

For the incompatible-schema case (full rebuild), the table OID changes.
This is unavoidable but is the same situation as today's drop + recreate.
We should emit a `WARNING` when this happens.

### 3.5 Lock considerations

The ALTER will acquire an `AccessExclusiveLock` on the storage table for the
duration of the schema migration (Phase 3) and full refresh (Phase 5). This
blocks concurrent reads. For large tables this could be significant.

**Future optimization:** For same-schema cases, we could use a CONCURRENTLY-
style approach — build the new data into a shadow table, then swap via
`ALTER TABLE ... RENAME` within a short lock window. This is out of scope for
the initial implementation but is the natural evolution.

---

## 4. Edge Cases

### 4.1 Query changes source tables (dependency migration)

Example: old query reads from `orders`, new query reads from `orders` +
`customers`. The ALTER must:
- Keep CDC triggers on `orders` (already exists).
- Create CDC triggers + change buffer on `customers` (new source).
- If a source is removed, clean up only if no other ST depends on it (same
  logic as `cleanup_cdc_for_source`).

### 4.2 Query changes the GROUP BY / row identity

When the grouping key changes, existing `__pgt_row_id` values are meaningless.
The full refresh in Phase 5 handles this correctly — it truncates and
re-inserts all rows. But the GROUP BY composite index needs to be rebuilt.

### 4.3 DIFFERENTIAL → FULL mode interaction

If the user changes both `query` and `refresh_mode` in the same ALTER call,
both are applied. The refresh mode migration (CDC ↔ IVM trigger swap)
happens in Phase 2/4 as part of the infrastructure teardown/setup.

### 4.4 TopK ↔ non-TopK transitions

- **Non-TopK → TopK:** The new query has ORDER BY + LIMIT. Store TopK metadata.
  The storage table schema is the same (just the base query columns), so this
  is typically a same-schema migration.
- **TopK → non-TopK:** Clear TopK metadata from catalog. Storage schema
  unchanged.

### 4.5 `__pgt_count` column transitions

- If the old query needed `__pgt_count` (aggregate) and the new query doesn't
  (flat SELECT), the column must be dropped.
- If the new query needs `__pgt_count` but the old didn't, the column must be
  added.
- Same logic for `__pgt_count_l` / `__pgt_count_r` (INTERSECT/EXCEPT).

### 4.6 Concurrent ALTER + scheduled refresh

Phase 1 suspends the ST, preventing the scheduler from running a refresh in
parallel. The scheduler checks `status` before each refresh and skips
SUSPENDED tables.

### 4.7 View inlining & original_query

Both `defining_query` (rewritten) and `original_query` (user-supplied) are
updated. If the new query references a view, the full inlining pipeline runs
and both variants are stored.

### 4.8 Diamond consistency groups

Changing a query may alter the ST's position in a diamond dependency group.
The DAG rebuild in Phase 4 handles this — the scheduler will recompute
diamond groups on next cycle.

---

## 5. Implementation Steps

### Step 1: Refactor `create_stream_table_impl` into reusable pieces
**File:** `src/api.rs`  
**Effort:** ~4 hours

Extract the following into standalone functions:

| Function | Purpose |
|---|---|
| `run_query_rewrite_pipeline(query) → String` | All 5 rewrites (view inlining, DISTINCT ON, GROUPING SETS, scalar subquery, SubLinks-in-OR) |
| `validate_and_parse_query(query, mode) → (Vec<ColumnDef>, Option<ParsedTree>, Option<TopkInfo>)` | Validation, DVM parse, TopK detection |
| `setup_storage_table(schema, name, columns, ...) → Oid` | CREATE TABLE + indexes |
| `insert_catalog_and_deps(st_meta, deps, ...) → i64` | Catalog + dependency insertion |
| `setup_cdc_infrastructure(deps, mode, ...) → ()` | CDC trigger + change buffer creation |

This refactoring benefits `create_stream_table_impl` too — it becomes a
straightforward pipeline of these functions.

### Step 2: Add schema comparison utility
**File:** `src/api.rs` (or new `src/schema_diff.rs`)  
**Effort:** ~2 hours

```rust
enum SchemaChange {
    Same,
    Compatible {
        added: Vec<ColumnDef>,
        removed: Vec<String>,
    },
    Incompatible {
        reason: String,
    },
}

fn classify_schema_change(
    old_columns: &[ColumnDef],
    new_columns: &[ColumnDef],
) -> SchemaChange
```

Compare by column name and type OID. A column whose name matches but type
changed checks `pg_cast` for an implicit cast — if castable, it's Compatible
with an ALTER COLUMN TYPE; if not, Incompatible.

### Step 3: Add storage table migration logic
**File:** `src/api.rs`  
**Effort:** ~3 hours

```rust
fn migrate_storage_table(
    schema: &str,
    name: &str,
    change: &SchemaChange,
    needs_pgt_count: bool,
    needs_dual_count: bool,
    needs_union_dedup: bool,
    new_columns: &[ColumnDef],
    group_by_cols: Option<&[String]>,
) -> Result<Oid, PgTrickleError>
```

For `Same`: no-op, return existing OID.  
For `Compatible`: issue `ALTER TABLE ADD/DROP COLUMN`, rebuild indexes.  
For `Incompatible`: `DROP TABLE CASCADE` + `CREATE TABLE`, return new OID.

### Step 4: Add dependency diffing utility
**File:** `src/api.rs` or `src/catalog.rs`  
**Effort:** ~1 hour

```rust
struct DependencyDiff {
    added: Vec<(Oid, String)>,     // (source_oid, source_type)
    removed: Vec<(Oid, String)>,
    kept: Vec<(Oid, String)>,
}

fn diff_dependencies(
    old: &[StDependency],
    new: &[(Oid, String)],
) -> DependencyDiff
```

### Step 5: Implement `alter_stream_table_query`
**File:** `src/api.rs`  
**Effort:** ~6 hours

The core logic, called from `alter_stream_table_impl` when `query` is
`Some(...)`. Orchestrates Phases 0–5, using the utilities from Steps 1–4.

### Step 6: Update SQL signature & upgrade script
**Files:** `src/api.rs` (pgrx attribute), `sql/pg_trickle--0.2.1--0.3.0.sql`  
**Effort:** ~1 hour

Add the `query` parameter to the `#[pg_extern]` function signature with
`default!(Option<&str>, "NULL")`. Write the upgrade migration that
`DROP FUNCTION` the old signature and recommits via `CREATE OR REPLACE`.

### Step 7: Update dbt materialization
**File:** `dbt-pgtrickle/macros/materializations/stream_table.sql`,
`dbt-pgtrickle/macros/adapters/alter_stream_table.sql`  
**Effort:** ~2 hours

When the defining query changes, call `alter_stream_table(name, query => ...)`
instead of drop + recreate. Fall back to drop + recreate only if the
`alter_stream_table` call fails (e.g., older pg_trickle version without
query support).

### Step 8: Update docs & FAQ
**Files:** `docs/SQL_REFERENCE.md`, `docs/FAQ.md`, `docs/ARCHITECTURE.md`  
**Effort:** ~2 hours

- Update the `alter_stream_table` parameter table.
- Remove the "must drop and recreate" guidance throughout the FAQ.
- Document same-schema vs. compatible vs. incompatible behavior.
- Update the dbt FAQ section.

### Step 9: Tests
**Files:** `tests/e2e_alter_query_tests.rs` (new), `src/api.rs` (unit tests)  
**Effort:** ~6 hours

| Test | Scenario |
|---|---|
| `test_alter_query_same_schema` | Change WHERE clause, same output columns |
| `test_alter_query_add_column` | Add a column to SELECT |
| `test_alter_query_remove_column` | Remove a column from SELECT |
| `test_alter_query_type_change_compatible` | Change `integer` → `bigint` |
| `test_alter_query_type_change_incompatible` | Change `integer` → `text` (full rebuild) |
| `test_alter_query_change_sources` | Old query uses table A, new uses A+B |
| `test_alter_query_remove_source` | Remove a source table |
| `test_alter_query_topk_transition` | Non-TopK → TopK and back |
| `test_alter_query_pgt_count_transition` | Flat query → aggregate query |
| `test_alter_query_differential_mode` | Verify CDC infra updated, delta cache flushed |
| `test_alter_query_immediate_mode` | Verify IVM triggers migrated |
| `test_alter_query_with_mode_change` | Change query + refresh mode simultaneously |
| `test_alter_query_cycle_detection` | New query introduces cycle → rejected |
| `test_alter_query_invalid_query` | Bad SQL → rejected, old ST untouched |
| `test_alter_query_concurrent_reads` | Verify no "relation does not exist" during alter |
| `test_alter_query_dbt_integration` | dbt materialization uses ALTER instead of drop/recreate |
| `test_alter_query_view_inlining` | New query references a view |

---

## 6. Alternatives Considered

### A. Deferred migration (async, via scheduler)

Instead of doing everything synchronously in `alter_stream_table`, just
update the catalog and set a new flag (`needs_query_migration`). The
scheduler picks it up on the next cycle and does the migration.

**Pros:** Faster ALTER call. User gets control back immediately.  
**Cons:** Data gap between ALTER and next scheduler cycle. More complex
scheduler logic. Harder to report errors synchronously. The dbt
materialization can't verify success inline.

**Decision:** Do it synchronously. The full refresh is already O(query
execution time) and that's what users expect from `ALTER ... query`.

### B. Shadow table swap (zero-lock approach)

Build new data into `pgtrickle._shadow_{name}`, then `ALTER TABLE RENAME`
in a short lock window.

**Pros:** Minimal read blocking time.  
**Cons:** Doubles storage during migration. More complex OID management.
Publications, views, and policies reference the old OID and need updating.

**Decision:** Out of scope for v1. Revisit when users report lock-duration
issues on large tables.

### C. Do nothing (keep drop + recreate)

**Pros:** No new code.  
**Cons:** All the problems listed in §1 remain. The dbt materialization
already works around this, but the data gap is inherent.

**Decision:** Rejected — this is the second most requested feature.

---

## 7. Risks & Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Long lock hold during full refresh of large tables | Read queries blocked | Document clearly; plan shadow-swap as future optimization |
| Incompatible schema change invalidates dependent views/policies | Cascading failures | Emit WARNING; document in UPGRADING.md |
| Bug in dependency diffing leaves orphaned CDC triggers | Change buffer bloat | Add `pgtrickle.cleanup_orphaned_cdc()` utility function |
| Partial failure mid-migration | Corrupted state | Entire ALTER runs in one SPI transaction; rollback on error |

---

## 8. Milestones

| Milestone | Steps | Est. Effort |
|---|---|---|
| M1: Refactor `create_stream_table_impl` | Step 1 | 4h |
| M2: Core ALTER QUERY implementation | Steps 2–5 | 12h |
| M3: SQL upgrade + dbt | Steps 6–7 | 3h |
| M4: Docs + tests | Steps 8–9 | 8h |
| **Total** | | **~27h** |
