# PLAN: View Inlining for Stream Tables

**Status:** Proposed  
**Date:** 2026-02-25  
**Branch:** `main`  
**Resolves:** SQL_GAPS_6 G2.1 (P0 — views as sources in DIFFERENTIAL mode)  
**Related:** SQL_GAPS_6 G2.2 (materialized views — handled separately)  
**Effort:** 8–12 hours (implementation + tests)

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Design Overview](#2-design-overview)
3. [Architecture Decision: Why Parse-Tree Rewrite](#3-architecture-decision-why-parse-tree-rewrite)
4. [Detailed Design](#4-detailed-design)
5. [Implementation Steps](#5-implementation-steps)
6. [Edge Cases & Constraints](#6-edge-cases--constraints)
7. [DDL Hook Integration](#7-ddl-hook-integration)
8. [Catalog Impact](#8-catalog-impact)
9. [Testing Plan](#9-testing-plan)
10. [Risk Assessment](#10-risk-assessment)
11. [Future Work](#11-future-work)

---

## 1. Problem Statement

When a user creates a stream table whose defining query references a **view**,
pg_stream silently fails to track changes in DIFFERENTIAL mode:

```sql
CREATE VIEW active_orders AS
  SELECT * FROM orders WHERE status = 'active';

-- Stream table referencing the view:
SELECT pgstream.create('order_summary', 'DIFFERENTIAL',
  'SELECT customer_id, COUNT(*) FROM active_orders GROUP BY customer_id');
```

**What happens today:**

1. `resolve_table_oid()` resolves `active_orders` to an OID (views have OIDs)
2. `parse_from_item()` creates `OpTree::Scan { table_oid, ... }` for the view
3. `extract_source_relations()` classifies the view as `source_type = "VIEW"`
4. CDC trigger loop at `api.rs:274` **skips** views: `if source_type == "TABLE"`
5. No triggers are created → changes to `orders` are invisible → the stream
   table becomes **permanently stale after initial population**

There is no error, no warning. The user sees a healthy stream table that never
updates. This is **P0 — silent data staleness**.

### Current CDC trigger skip behavior

```rust
// api.rs lines 273-277
for (source_oid, source_type) in &source_relids {
    if source_type == "TABLE" {
        setup_cdc_for_source(*source_oid, pgs_id, &change_schema)?;
    }
}
```

---

## 2. Design Overview

**View inlining** transparently replaces view references in the defining query
with the view's underlying SELECT definition as a subquery. After inlining,
the query references only base tables, so CDC triggers land on the correct
targets.

```sql
-- Before inlining:
SELECT customer_id, COUNT(*) FROM active_orders GROUP BY customer_id

-- After inlining:
SELECT customer_id, COUNT(*)
FROM (SELECT orders.order_id, orders.customer_id, orders.status, orders.amount
      FROM orders
      WHERE status = 'active') AS active_orders
GROUP BY customer_id
```

This is implemented as **auto-rewrite pass #0** — the first rewrite in the
existing chain, before DISTINCT ON, GROUPING SETS, etc. This ensures:

1. View definitions containing DISTINCT ON, GROUPING SETS, etc. get further
   rewritten by the downstream passes
2. `extract_source_relations()` sees only base tables (`source_type = "TABLE"`)
3. CDC triggers are created on the actual base tables
4. The DVM parser builds `OpTree::Scan` nodes for base tables with real PKs

### Data flow after this change

```
                    ┌── rewrite_views_inline()  ← NEW (pass #0)
                    │
User SQL ──────────>├── rewrite_distinct_on()
                    ├── rewrite_grouping_sets()
                    ├── rewrite_scalar_subquery_in_where()
                    ├── rewrite_sublinks_in_or()
                    ├── rewrite_multi_partition_windows()
                    │
                    ├── validate_defining_query()
                    ├── reject_limit_offset()
                    ├── reject_unsupported_constructs()
                    ├── parse_defining_query_full()
                    │
                    ├── extract_source_relations()  ← sees base tables only
                    ├── setup_cdc_for_source()      ← triggers on base tables
                    └── catalog insert
```

---

## 3. Architecture Decision: Why Parse-Tree Rewrite

### Rejected alternatives

| Approach | Description | Why rejected |
|----------|-------------|-------------|
| **Reject views** (G2.1 Option A) | Return error when view detected | Unnecessarily restrictive; views are a common SQL abstraction |
| **Auto-downgrade to FULL** (G2.1 Option C) | Detect view, switch to FULL mode | Performance penalty; doesn't solve the underlying problem |
| **OpTree-level expansion** | Expand at `parse_from_item()` time | Too late — `extract_source_relations()` runs on the **original** query string, not the OpTree. Also complicates the DVM tree. |
| **Regex string replacement** | `s/view_name/(view_def)/` | Fragile — doesn't handle schema qualification, aliases, quoting, nested views |

### Chosen: Parse-tree-level SQL rewrite (Option B+)

Follows the exact pattern of the 5 existing rewrites:

1. Call `pg_sys::raw_parser()` on the query string
2. Walk the `SelectStmt.fromClause` looking for `RangeVar` nodes
3. For each `RangeVar`, resolve OID → check `relkind` in `pg_class`
4. If `relkind = 'v'`: get definition via `pg_get_viewdef(oid, true)`
5. Replace the `RangeVar` with `(view_definition) AS alias` in the deparsed SQL
6. Return the rewritten SQL string

**Why this is robust:**

- View definitions from `pg_get_viewdef()` use fully-qualified table names
- PostgreSQL's own pretty-printer handles all SQL syntax correctly
- Column aliases are preserved through the subquery alias
- Nested views are handled by iterating until fixpoint
- All existing deparse infrastructure (`deparse_select_stmt_to_sql`,
  `deparse_from_item_to_sql`) is reusable

---

## 4. Detailed Design

### 4.1 Core function: `rewrite_views_inline()`

```rust
// src/dvm/parser.rs

/// Auto-rewrite pass #0: Replace view references with inline subqueries.
///
/// For each RangeVar in the FROM clause that resolves to a PostgreSQL view
/// (relkind='v'), replaces it with `(view_definition) AS alias`.
/// Handles nested views by iterating until no views remain (fixpoint).
///
/// Materialized views (relkind='m') are NOT inlined — their semantics
/// differ (stale snapshot vs live query). They are rejected later.
pub fn rewrite_views_inline(query: &str) -> Result<String, PgStreamError> {
    let mut current = query.to_string();
    let max_depth = 10;  // Guard against pathological nesting
    
    for depth in 0..max_depth {
        let rewritten = rewrite_views_inline_once(&current)?;
        if rewritten == current {
            return Ok(current);  // Fixpoint reached — no more views
        }
        current = rewritten;
    }
    
    Err(PgStreamError::QueryParseError(format!(
        "View inlining exceeded maximum nesting depth of {}. \
         This may indicate circular view dependencies.",
        max_depth
    )))
}
```

### 4.2 Single-pass rewrite: `rewrite_views_inline_once()`

Algorithm for one pass:

1. Parse the query with `raw_parser()`
2. Extract the `SelectStmt`
3. Handle set operations: if `op != SETOP_NONE`, recurse into `larg`/`rarg`
4. Walk the `fromClause` list
5. For each node:
   - If `RangeVar`: resolve OID → check relkind → if 'v', get view def
   - If `JoinExpr`: recurse into `larg` and `rarg`
   - If `RangeSubselect`: inspect inner `SelectStmt` for view refs
6. If any views found, deparse the modified query back to SQL
7. If no views found, return the original string unchanged

### 4.3 View definition retrieval

```sql
SELECT pg_get_viewdef(oid, true)  -- pretty-print = true
```

`pg_get_viewdef(oid, true)` returns the view's stored query with:
- Fully-qualified table names (schema.table)
- Explicit column lists (no `*` expansion in the definition itself,
  though the stored definition preserves the original form)
- Cleaned-up formatting

### 4.4 Alias preservation

Critical for correctness: the view's alias (or implicit name) must be
preserved so column references in the outer query still resolve.

```sql
-- Input: SELECT v.x FROM my_view AS v WHERE v.y > 5
-- Output: SELECT v.x FROM (SELECT ...) AS v WHERE v.y > 5

-- Input: SELECT my_view.x FROM my_view  (implicit alias = view name)
-- Output: SELECT my_view.x FROM (SELECT ...) AS my_view
```

Rules:
1. If `rv.alias` is non-NULL: use the explicit alias
2. If `rv.alias` is NULL: use `rv.relname` (the view name) as alias

### 4.5 Column alias handling

Special case: `SELECT * FROM my_view`. PostgreSQL expands `*` to the view's
output columns. After inlining, the subquery's output columns must match
the view's column names.

`pg_get_viewdef()` returns the view definition with the view's output column
names. For example:

```sql
CREATE VIEW v(a, b) AS SELECT x, y FROM t;
-- pg_get_viewdef returns: SELECT t.x AS a, t.y AS b FROM t
```

However, pg_get_viewdef does NOT always alias columns. It returns the original
definition as-is. So if the view was `CREATE VIEW v AS SELECT x, y FROM t`,
it returns `SELECT x, y FROM t` — meaning the subquery alias columns are
`x` and `y`, not renamed.

This is correct because PostgreSQL creates the view's column list from the
SELECT output. The original names are preserved through the subquery alias.

### 4.6 CTE interaction

If the defining query has CTEs that reference views, those are handled
automatically: the rewrite walks the full `SelectStmt` including its
`withClause` CTE bodies.

If a **view definition** itself contains CTEs, that's fine — the inline
subquery becomes `(WITH ... SELECT ... FROM ...) AS alias`, which
PostgreSQL 14+ supports (CTEs in subqueries).

For PostgreSQL versions before 14 where CTEs in subqueries aren't
supported: pg_stream targets PG 18, so this is not a concern.

### 4.7 Handling views in JOINs

Views can appear on either side of a JOIN:

```sql
SELECT t.x, v.y FROM table1 t JOIN my_view v ON t.id = v.id
```

The rewrite walks `JoinExpr.larg` and `JoinExpr.rarg` recursively, so
views in any join position are inlined.

### 4.8 LATERAL interaction

```sql
SELECT t.*, l.* FROM table1 t, LATERAL (SELECT * FROM my_view WHERE my_view.id = t.id) AS l
```

The view reference inside the LATERAL subquery is within a `RangeSubselect`
node. The rewrite recurses into subselect bodies, so this works. The outer
column reference (`t.id`) remains valid because the subquery structure is
preserved.

However: if the view **itself** contains a LATERAL reference that depends
on outer columns, inlining is still correct because the view definition
is placed as-is inside the subquery wrapper.

---

## 5. Implementation Steps

### Step 1: Implement `rewrite_views_inline()` in `parser.rs`

**File:** `src/dvm/parser.rs`  
**Location:** Before `rewrite_distinct_on()` (around line 1854)  
**Effort:** 3–4 hours

Functions to create:

| Function | Purpose |
|----------|---------|
| `rewrite_views_inline(query)` | Outer loop — iterates until fixpoint |
| `rewrite_views_inline_once(query)` | Single pass — parse, walk, replace, deparse |
| `resolve_relkind(schema, table)` | SPI lookup: `SELECT relkind FROM pg_class ...` |
| `get_view_definition(schema, table)` | SPI call: `pg_get_viewdef(oid, true)` |

The deparse step reuses the existing `deparse_select_stmt_to_sql()`,
`deparse_from_item_to_sql()`, and related functions. The key modification
is that when a RangeVar is identified as a view, instead of deparsing it
as `"schema"."view_name"`, it's deparsed as `(view_definition) AS alias`.

**Implementation approach — deparse with substitution:**

Rather than mutating the parse tree in-place (which is complex with raw
C pointers), the implementation will:

1. Walk the `fromClause` to identify which RangeVars are views
2. Build a substitution map: `(schema, name) → (view_sql, alias)`
3. Deparse the full query using modified deparse functions that check
   the substitution map when processing RangeVar nodes

This avoids any `unsafe` mutation of the parse tree while reusing the
existing deparse infrastructure.

### Step 2: Export from `dvm/mod.rs`

**File:** `src/dvm/mod.rs`  
**Change:** Add `rewrite_views_inline` to the `pub use parser::{ ... }` export list  
**Effort:** 1 minute

```rust
pub use parser::{
    parse_defining_query, parse_defining_query_full, reject_limit_offset,
    reject_unsupported_constructs, rewrite_distinct_on, rewrite_grouping_sets,
    rewrite_multi_partition_windows, rewrite_scalar_subquery_in_where,
    rewrite_sublinks_in_or, rewrite_views_inline,  // ← NEW
    ...
};
```

### Step 3: Wire into the rewrite chain in `api.rs`

**File:** `src/api.rs`  
**Location:** `create_stream_table_impl()`, before `rewrite_distinct_on()` 
(line 68)  
**Effort:** 15 minutes

```rust
// ── View inlining auto-rewrite ─────────────────────────────────
// Views are replaced with their underlying SELECT definition as
// inline subqueries. This ensures CDC triggers land on base tables
// and the DVM parser sees real table scans with PKs. Must run first
// so view definitions get further rewritten by downstream passes.
let query = &crate::dvm::rewrite_views_inline(query)?;

// ── DISTINCT ON auto-rewrite ───────────────────────────────────
let query = &crate::dvm::rewrite_distinct_on(query)?;
// ... rest of chain unchanged ...
```

### Step 4: Reject materialized views

**File:** `src/api.rs` or `src/dvm/parser.rs`  
**Location:** In the view inlining function, when `relkind = 'm'` is detected  
**Effort:** 30 minutes

When a `RangeVar` resolves to a materialized view (`relkind = 'm'`), the
rewrite should **not** inline it — instead, it should return an error:

```rust
if relkind == "m" {
    return Err(PgStreamError::UnsupportedOperator(format!(
        "Materialized view '{}' cannot be used as a source in DIFFERENTIAL mode. \
         Materialized views are stale snapshots — CDC triggers cannot track \
         REFRESH MATERIALIZED VIEW. Use the underlying query directly, or \
         switch to FULL refresh mode.",
        view_name
    )));
}
```

For FULL mode, materialized views are fine (no CDC needed) — but since
the rewrite runs before mode checking, we need to either:

**(a)** Pass the refresh mode into the rewrite function, or  
**(b)** Only reject in DIFFERENTIAL mode during the later validation phase

**Recommended:** Option (b) — keep the rewrite idempotent and mode-unaware.
Add a separate validation step after the rewrite chain that checks if any
materialized views were found that couldn't be inlined:

```rust
// After rewrites, before parse_defining_query_full():
if refresh_mode == RefreshMode::Differential {
    crate::dvm::reject_materialized_views(query)?;
}
```

### Step 5: Add foreign table rejection

**File:** Same as Step 4  
**Effort:** 15 minutes

Also reject `relkind = 'f'` (foreign tables) with a clear message:

```rust
if relkind == "f" {
    return Err(PgStreamError::UnsupportedOperator(format!(
        "Foreign table '{}' cannot be used as a source in DIFFERENTIAL mode. \
         Row-level triggers cannot be created on foreign tables. \
         Use FULL refresh mode instead.",
        table_name
    )));
}
```

### Step 6: Add view DDL tracking to hooks

**File:** `src/hooks.rs`  
**Location:** `handle_ddl_command()` match block  
**Effort:** 1–2 hours

Add a case for view DDL:

```rust
// ── View DDL ──────────────────────────────────────────────────
("view", "CREATE VIEW") | ("view", "ALTER VIEW") => {
    handle_view_change(cmd);
}
```

The `handle_view_change()` function needs to:

1. Resolve the view OID from `cmd.objid`
2. Get all **base tables** the view depends on (walk `pg_depend`)
3. For each base table, find stream tables that depend on it
4. Mark affected stream tables for reinit (`needs_reinit = true`)

This handles `CREATE OR REPLACE VIEW` which changes a view definition.
After reinit, the view inlining rewrite runs again with the new definition.

**Important:** The hook also needs to track view drops. When a view is
dropped, any stream table whose **original** defining query referenced
that view can no longer be re-created. This is handled in Step 8.

### Step 7: Store original query in catalog

**File:** `src/lib.rs` (schema), `src/catalog.rs`, `src/api.rs`  
**Effort:** 1–2 hours

Add an `original_query` column to preserve the user's original SQL:

```sql
ALTER TABLE pgstream.pgs_stream_tables
  ADD COLUMN original_query TEXT;
```

**Schema change in `src/lib.rs`:**

```sql
CREATE TABLE IF NOT EXISTS pgstream.pgs_stream_tables (
    ...
    defining_query  TEXT NOT NULL,    -- Rewritten (post-inlining)
    original_query  TEXT,             -- Original user SQL (pre-inlining)
    ...
);
```

**Why both:** The `defining_query` is used by the refresh engine (needs
the expanded form). The `original_query` is needed for:

1. **Reinit after view definition change:** Re-run the rewrite pipeline
   on the original query to pick up the new view definition
2. **User introspection:** `pgstream.info()` shows what the user wrote
3. **ALTER stream table:** If we ever support changing the defining query

**In `api.rs`:**

```rust
// Store both the original and rewritten query
StreamTableMeta::insert(
    pgs_relid, &table_name, &schema,
    query,             // defining_query (rewritten)
    Some(original),    // original_query (user's input)
    schedule_str, refresh_mode,
)?;
```

### Step 8: Add view dependency tracking

**File:** `src/api.rs` (dependency registration), `src/catalog.rs`  
**Effort:** 1 hour

Even though the **rewritten** query only references base tables, we need to
track which views were inlined so that:

1. View DDL changes trigger reinit (Step 6)
2. View drops are detected and reported

**Approach:** After inlining, call `extract_source_relations` on **both**
the original and rewritten queries. Register base table dependencies from
the rewritten query (for CDC), and register view "soft dependencies" from
the original-only relations (for DDL tracking):

```rust
let rewritten_sources = extract_source_relations(rewritten_query)?;
let original_sources = extract_source_relations(original_query)?;

// Views appear in original but not rewritten (they were inlined)
let view_sources: Vec<_> = original_sources.iter()
    .filter(|(_, stype)| stype == "VIEW")
    .collect();

// Register base table dependencies (from rewritten query)
for (source_oid, source_type) in &rewritten_sources { ... }

// Register view soft-dependencies (for DDL tracking only)
for (view_oid, _) in &view_sources {
    StDependency::insert_with_snapshot(
        pgs_id, *view_oid, "VIEW", None, None, None,
    )?;
}
```

The view dependency rows enable `find_downstream_pgs_ids()` to find
affected stream tables when a view is modified.

### Step 9: Write tests

**Effort:** 2–3 hours  
See [Section 9: Testing Plan](#9-testing-plan) for details.

---

## 6. Edge Cases & Constraints

### 6.1 Nested views (view → view → table)

Handled by the fixpoint loop in `rewrite_views_inline()`. Each pass
inlines one level of views. A view referencing another view becomes a
query referencing a view after the first pass, which gets inlined in
the second pass. Limited to 10 iterations to prevent runaway loops
(PostgreSQL prevents circular view dependencies, so this is a safety
net, not a real concern).

**Example:**

```sql
CREATE VIEW v1 AS SELECT * FROM base_table WHERE x > 0;
CREATE VIEW v2 AS SELECT * FROM v1 WHERE y < 100;

-- User's query:
SELECT COUNT(*) FROM v2

-- After pass 1:
SELECT COUNT(*) FROM (SELECT * FROM v1 WHERE y < 100) AS v2

-- After pass 2:
SELECT COUNT(*) FROM (SELECT * FROM (SELECT * FROM base_table WHERE x > 0) AS v1 WHERE y < 100) AS v2
```

### 6.2 Views with CTEs

`pg_get_viewdef()` returns the full CTE definition. Example:

```sql
CREATE VIEW v AS WITH cte AS (SELECT ...) SELECT * FROM cte;
-- pg_get_viewdef returns: WITH cte AS (SELECT ...) SELECT cte.col FROM cte
```

When inlined: `FROM (WITH cte AS (...) SELECT ...) AS v`. PostgreSQL 14+
supports CTEs inside subqueries. Since pg_stream targets PG 18, this works.

### 6.3 Views with set operations

```sql
CREATE VIEW v AS SELECT * FROM t1 UNION ALL SELECT * FROM t2;
-- pg_get_viewdef returns: SELECT t1.col FROM t1 UNION ALL SELECT t2.col FROM t2
```

When inlined: `FROM (SELECT ... UNION ALL SELECT ...) AS v`. Valid SQL.

### 6.4 Views with SECURITY DEFINER

`pg_get_viewdef()` returns the query regardless of security settings.
However, after inlining, the query executes with the **current user's**
permissions, not the view definer's. This could cause permission errors
if the user lacks direct access to base tables.

**Handling:** If the user can `SELECT` through the view but not directly
from the base tables, the `validate_defining_query()` step (which runs
`LIMIT 0` on the rewritten query) will catch the permission error and
report it clearly. No special handling needed — the error message will be:

```
ERROR: permission denied for table <base_table>
HINT: The view '<view_name>' was expanded inline. You need SELECT privilege
      on the underlying tables, or use FULL refresh mode.
```

We should wrap the validation error to add this hint.

### 6.5 Views with column renaming

```sql
CREATE VIEW v(a, b) AS SELECT x, y FROM t;
```

`pg_get_viewdef(oid, true)` returns `SELECT t.x AS a, t.y AS b FROM t`,
which correctly maps the view's column names. No special handling needed.

### 6.6 Views in non-public schemas

`pg_get_viewdef()` returns schema-qualified references:

```sql
CREATE VIEW myschema.v AS SELECT * FROM myschema.t;
-- Returns: SELECT t.id, t.name FROM myschema.t
```

The rewrite resolves the view's schema from the `RangeVar.schemaname`
or defaults to `search_path` resolution. No special handling needed.

### 6.7 Views with `*` (wildcard) expansion

The user's defining query may use `SELECT * FROM my_view`. After inlining:

```sql
-- Before: SELECT * FROM my_view
-- After:  SELECT * FROM (SELECT t.col1, t.col2, t.col3 FROM t) AS my_view
```

The wildcard `*` in the outer query resolves to the subquery's output
columns. `pg_get_viewdef` returns explicit column lists, so this works.

### 6.8 Views in the `fromClause` of set operations

If the user writes:

```sql
SELECT * FROM v1 UNION ALL SELECT * FROM v2
```

The rewrite must handle set operations by recursing into `larg` and `rarg`
of the `SelectStmt` when `op != SETOP_NONE`. Each arm may reference views.

### 6.9 Recursive CTEs referencing views

```sql
WITH RECURSIVE r AS (
    SELECT * FROM my_view WHERE id = 1          -- base case
    UNION ALL
    SELECT v.* FROM my_view v JOIN r ON v.parent = r.id  -- recursive
)
SELECT * FROM r;
```

The view inlining walks CTE bodies via the `withClause`, so views inside
CTEs are inlined. The recursive self-reference (`r`) is not a view and
won't be touched.

### 6.10 Performance consideration

View inlining adds SPI calls (`relkind` lookup + `pg_get_viewdef`) at
stream table **creation time** only. There is zero runtime overhead during
refresh — the stored `defining_query` is already expanded.

For deeply nested views (e.g., 5 levels), the fixpoint loop runs 5+1
iterations, each calling `raw_parser()`. This is negligible (< 10ms).

---

## 7. DDL Hook Integration

### 7.1 View creation / replacement

When `CREATE OR REPLACE VIEW` changes a view that was inlined into a
stream table, the stream table's stored `defining_query` becomes stale.

**Detection mechanism:**

1. Hook fires for `("view", "CREATE VIEW")`
2. Look up the view OID in `pgs_dependencies` (source_type = 'VIEW')
3. If matches found: mark those stream tables as `needs_reinit = true`
4. On next scheduled refresh, the reinit process:
   - Reads `original_query` from catalog
   - Re-runs the full rewrite pipeline (including view inlining)
   - Rebuilds CDC triggers for the (possibly different) base tables
   - Repopulates the stream table

### 7.2 View drop

When a view is dropped, any stream table that referenced it is broken.

**Detection mechanism:**

1. Extend `pg_stream_on_sql_drop()` to also handle `object_type == "view"`
2. Look up the dropped OID in `pgs_dependencies`
3. If matches found: mark as `needs_reinit = true` with error status

The reinit will fail (view no longer exists) and the stream table enters
ERROR status with a clear message. The user must `ALTER` the defining
query or drop the stream table.

### 7.3 Base table DDL (existing functionality)

The existing `handle_alter_table()` hook already handles DDL on base
tables. After view inlining, base tables are registered as dependencies,
so ALTER TABLE on a base table correctly triggers the existing
column-change detection and reinit logic.

### 7.4 Hook code structure

```rust
// hooks.rs — additions to handle_ddl_command()

("view", tag) if tag == "CREATE VIEW" || tag == "ALTER VIEW" => {
    handle_view_change(cmd);
}

// hooks.rs — new function
fn handle_view_change(cmd: &DdlCommand) {
    let identity = cmd.object_identity.as_deref().unwrap_or("unknown");
    
    // Find STs that depend on this view
    let affected = match find_downstream_pgs_ids(cmd.objid) {
        Ok(ids) => ids,
        Err(e) => {
            pgrx::warning!(
                "pg_stream_ddl_tracker: failed to find dependents of view {}: {}",
                identity, e
            );
            return;
        }
    };
    
    if affected.is_empty() {
        return;
    }
    
    pgrx::info!(
        "pg_stream: view {} changed, marking {} stream table(s) for reinit",
        identity, affected.len()
    );
    
    for pgs_id in &affected {
        if let Err(e) = StreamTableMeta::mark_needs_reinit(*pgs_id) {
            pgrx::warning!("pg_stream: failed to mark ST {} for reinit: {}", pgs_id, e);
        }
    }
}
```

And in the drop handler:

```rust
// hooks.rs — extend pg_stream_on_sql_drop()
for obj in &dropped {
    match obj.object_type.as_str() {
        "table" => handle_dropped_table(obj),
        "view" => handle_dropped_view(obj),  // NEW
        _ => {}
    }
}
```

---

## 8. Catalog Impact

### 8.1 Schema migration

Add `original_query` column:

```sql
-- Extension upgrade SQL (0.1.x → 0.2.0 or similar)
ALTER TABLE pgstream.pgs_stream_tables
  ADD COLUMN IF NOT EXISTS original_query TEXT;

-- Backfill: for existing STs, original = defining (no views were inlined)
UPDATE pgstream.pgs_stream_tables
SET original_query = defining_query
WHERE original_query IS NULL;
```

### 8.2 Catalog struct update

```rust
// catalog.rs — StreamTableMeta
pub struct StreamTableMeta {
    // ... existing fields ...
    pub original_query: Option<String>,  // NEW
}
```

### 8.3 Dependency table — no schema change needed

The `pgs_dependencies` table already supports `source_type = 'VIEW'`.
View soft-dependencies use existing infrastructure.

### 8.4 Info view update

The `pgstream.stream_tables` view should expose `original_query`:

```sql
CREATE OR REPLACE VIEW pgstream.stream_tables AS
SELECT
    st.pgs_id,
    st.pgs_schema || '.' || st.pgs_name AS name,
    st.defining_query,
    st.original_query,  -- NEW
    ...
FROM pgstream.pgs_stream_tables st;
```

---

## 9. Testing Plan

### 9.1 Unit tests (`src/dvm/parser.rs`)

| Test | Description |
|------|-------------|
| `test_rewrite_views_inline_simple_view` | Single view → base table expansion |
| `test_rewrite_views_inline_no_views` | Query with only tables → unchanged |
| `test_rewrite_views_inline_aliased_view` | `FROM v AS alias` preserves alias |
| `test_rewrite_views_inline_unaliased_view` | `FROM v` uses view name as alias |
| `test_rewrite_views_inline_nested_views` | v2 → v1 → t, both levels inlined |
| `test_rewrite_views_inline_view_in_join` | `t1 JOIN v ON ...` |
| `test_rewrite_views_inline_view_both_sides` | `v1 JOIN v2 ON ...` |
| `test_rewrite_views_inline_view_with_cte` | View definition contains CTE |
| `test_rewrite_views_inline_view_with_union` | View definition is UNION ALL |
| `test_rewrite_views_inline_schema_qualified` | `myschema.my_view` |
| `test_rewrite_views_inline_matview_untouched` | Materialized view not inlined |
| `test_rewrite_views_inline_mixed` | Table + view + subquery in FROM |
| `test_rewrite_views_inline_depth_limit` | Exceeds max depth → clear error |
| `test_rewrite_views_inline_column_aliases` | `CREATE VIEW v(a,b) AS ...` |

**Note:** Unit tests require a PG backend (SPI access for `relkind` lookup
and `pg_get_viewdef`). These will likely be `#[pg_test]` tests.

### 9.2 E2E tests (`tests/e2e_view_tests.rs` — new file)

| Test | Description |
|------|-------------|
| `test_view_inline_diff_basic` | Create view, create DIFF ST referencing it, INSERT into base → verify refresh captures change |
| `test_view_inline_diff_update_delete` | INSERT, UPDATE, DELETE through base table → all captured |
| `test_view_inline_diff_with_filter` | View has WHERE clause, verify filter semantics preserved |
| `test_view_inline_diff_with_aggregation` | `SELECT COUNT(*) FROM my_view GROUP BY ...` |
| `test_view_inline_diff_with_join` | View joined with table |
| `test_view_inline_diff_two_views` | Both sources are views |
| `test_view_inline_nested_view` | View referencing another view |
| `test_view_inline_full_mode` | FULL mode with view — should work without inlining too |
| `test_view_inline_matview_rejected` | Materialized view → clear error in DIFF |
| `test_view_inline_foreign_table_rejected` | Foreign table → clear error in DIFF |
| `test_view_inline_view_replaced` | CREATE OR REPLACE VIEW → reinit triggered |
| `test_view_inline_view_dropped` | DROP VIEW → error status |
| `test_view_inline_security_definer` | View with SECURITY DEFINER owner |
| `test_view_inline_truncate_base` | TRUNCATE on base table through view → captured |
| `test_view_inline_column_renamed` | View renames columns: `v(a,b)` |

### 9.3 Integration tests (if needed)

May not need dedicated integration tests — the E2E tests cover the full
flow. If SECURITY DEFINER testing requires multi-role setup, it belongs
in E2E.

---

## 10. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **View definition syntax not fully deparsed** | Low | High (incorrect SQL) | Use `pg_get_viewdef()` which is PostgreSQL's own pretty-printer; heavily battle-tested |
| **SECURITY DEFINER permission change** | Medium | Medium (unexpected errors) | Clear error message with HINT; validate_defining_query catches this |
| **Deeply nested views cause slow creation** | Very low | Low (one-time cost) | Depth limit of 10; each iteration is < 2ms |
| **View with INSTEAD OF triggers** | Low | Low (semantic difference) | INSTEAD OF triggers on views are irrelevant for the SELECT definition |
| **Set-returning view definitions** | Low | Low | PostgreSQL doesn't allow SRFs in view definitions that break subquery wrapping |
| **Regression in existing non-view queries** | Low | High | Rewrite returns input unchanged when no views found; extensive existing test suite |
| **Catalog migration breaks upgrade** | Low | Medium | Use `ADD COLUMN IF NOT EXISTS` + backfill |

---

## 11. Future Work

### 11.1 Materialized view live expansion (P3)

A future enhancement could offer `LIVE` mode for materialized views:
inline the matview's definition like a regular view, ignoring the snapshot
semantics. This would require a user opt-in flag.

### 11.2 View metadata in pgstream.info()

Display inlined views and their definitions in the monitoring output:

```sql
SELECT * FROM pgstream.info('my_st');
-- Output includes:
--   original_query: SELECT ... FROM my_view
--   defining_query: SELECT ... FROM (SELECT ... FROM base_table) AS my_view
--   inlined_views: [my_view → public.base_table]
```

### 11.3 Selective view re-expansion on reinit

When a view definition changes and reinit triggers, only re-expand the
changed view instead of re-running the full pipeline. This is an
optimization for stream tables with many view dependencies.

### 11.4 View dependency graph visualization

Extend the DAG visualization to show which views were inlined and their
relationships to base tables.

---

## Appendix A: Affected Files Summary

| File | Changes |
|------|---------|
| `src/dvm/parser.rs` | New: `rewrite_views_inline()`, `rewrite_views_inline_once()`, `resolve_relkind()`, `get_view_definition()`, `reject_materialized_views()` |
| `src/dvm/mod.rs` | Export: `rewrite_views_inline`, `reject_materialized_views` |
| `src/api.rs` | Wire rewrite pass #0; store `original_query`; register view deps |
| `src/hooks.rs` | Add view DDL handling: `handle_view_change()`, `handle_dropped_view()` |
| `src/lib.rs` | Schema: add `original_query` column |
| `src/catalog.rs` | `StreamTableMeta`: add `original_query` field; update insert/select |
| `tests/e2e_view_tests.rs` | New E2E test file: 15 tests |

## Appendix B: Execution Order

```
Session 1 (4h): Steps 1-3 — core rewrite + wiring + basic unit tests
Session 2 (3h): Steps 4-5 — matview/foreign table rejection + Step 7 catalog
Session 3 (2h): Steps 6, 8 — DDL hooks + view dep tracking
Session 4 (3h): Step 9 — full E2E test suite
```

Total: **~12 hours** (includes testing)
