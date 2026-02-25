# Plan: Native PostgreSQL Syntax for Stream Tables

Date: 2026-02-25
Status: PROPOSED
Last Updated: 2026-02-25

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Motivation](#motivation)
- [Constraints](#constraints)
- [Approach Evaluation](#approach-evaluation)
  - [Approach A — Parser Fork (Rejected)](#approach-a--parser-fork-rejected)
  - [Approach B — Table Access Method (Rejected)](#approach-b--table-access-method-rejected)
  - [Approach C — Foreign Data Wrapper (Rejected)](#approach-c--foreign-data-wrapper-rejected)
  - [Approach D — ProcessUtility_hook + CREATE MATERIALIZED VIEW (Recommended)](#approach-d--processutility_hook--create-materialized-view-recommended)
  - [Approach E — CALL Procedure Syntax (Complementary)](#approach-e--call-procedure-syntax-complementary)
- [Recommended Strategy: Tiered Syntax](#recommended-strategy-tiered-syntax)
- [Detailed Design — Tier 2: ProcessUtility_hook](#detailed-design--tier-2-processutility_hook)
  - [User-Facing Syntax](#user-facing-syntax)
  - [Hook Registration](#hook-registration)
  - [CREATE Interception](#create-interception)
  - [DROP Interception](#drop-interception)
  - [REFRESH Interception](#refresh-interception)
  - [ALTER Interception](#alter-interception)
  - [psql Tab-Completion Compatibility](#psql-tab-completion-compatibility)
  - [pg_dump / pg_restore Strategy](#pg_dump--pg_restore-strategy)
  - [Error Handling](#error-handling)
- [Detailed Design — Tier 1.5: CALL Procedure Syntax](#detailed-design--tier-15-call-procedure-syntax)
- [Implementation Phases](#implementation-phases)
- [Module Layout Changes](#module-layout-changes)
- [ADR: Native Syntax Approach](#adr-native-syntax-approach)
- [Testing Strategy](#testing-strategy)
- [Risks and Mitigations](#risks-and-mitigations)
- [Comparison with Prior Art](#comparison-with-prior-art)
- [References](#references)

---

## Executive Summary

This plan proposes a **tiered syntax strategy** for creating and managing
stream tables in pg_stream, evolving from the current function-call API toward
a more native-feeling DDL experience without forking PostgreSQL.

| Tier | Syntax | Status | Requires |
|------|--------|--------|----------|
| **Tier 1** | `SELECT pgstream.create_stream_table(...)` | Existing | Nothing extra |
| **Tier 1.5** | `CALL pgstream.create_stream_table(...)` | New (trivial) | PG 11+ |
| **Tier 2** | `CREATE MATERIALIZED VIEW ... WITH (pgstream.stream = true) AS ...` | New | `shared_preload_libraries` |

All tiers produce identical results — a stream table registered in
`pgstream.pgs_stream_tables` with CDC, DAG, and scheduling fully configured.
Tier 1 remains the stable, always-available interface. Tier 2 provides
native-feeling DDL syntax for users who prefer standard SQL idioms.

**Estimated effort:** ~1,200–1,800 lines of Rust (hook + tests) for Tier 2.

---

## Motivation

### Problem

The current API requires function-call syntax:

```sql
SELECT pgstream.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region',
    '1m',
    'DIFFERENTIAL'
);
```

While functional, this has several UX shortcomings:

1. **Unfamiliar idiom** — PostgreSQL users expect DDL operations (`CREATE`,
   `DROP`, `ALTER`) for persistent database objects. `SELECT function()`
   reads as a query, not a definition.

2. **Query-as-string** — The defining query is embedded as a text literal,
   losing IDE support (syntax highlighting, auto-complete, error detection).

3. **No psql `\d` integration** — Stream tables appear as plain tables in
   `\dt`, with no indication they are maintained views.

4. **pg_dump gap** — Function calls are not captured by `pg_dump`. Stream
   table definitions survive only via the catalog tables, requiring custom
   backup/restore tooling.

5. **Discoverability** — New users searching for "create materialized view
   postgres" won't find pg_stream's function-based approach.

### Goal

Provide a syntax that:
- Feels like native PostgreSQL DDL
- Preserves the query as SQL (not a string literal)
- Works with existing PostgreSQL tooling where possible
- Does NOT require forking PostgreSQL
- Maintains full backward compatibility with the function API

---

## Constraints

### Hard Constraints

1. **PostgreSQL's parser is not extensible.** There is no parser hook. The raw
   parser (`gram.y`) is compiled into the server binary. Extensions cannot add
   new keywords (`STREAM`) or grammar productions (`CREATE STREAM TABLE`).
   This rules out true `CREATE STREAM TABLE` syntax.

2. **`shared_preload_libraries` is already required** for the background
   scheduler worker and shared memory. The `ProcessUtility_hook` falls within
   the same requirement — no additional deployment burden.

3. **Hook chaining** — Other extensions (TimescaleDB, Citus, pg_stat_statements)
   may also set `ProcessUtility_hook`. Our hook MUST chain to the previous hook
   to avoid breaking other extensions.

4. **Backward compatibility** — The existing function API (`pgstream.create_stream_table()`)
   must continue to work unchanged. The new syntax is an additional path,
   not a replacement.

### Soft Constraints

5. **pg_dump compatibility** — Desirable but not expected to be perfect. A
   companion `pgstream.dump_stream_tables()` function is acceptable.

6. **Minimal surface area** — The hook should intercept only the DDL commands
   it needs and pass everything else through without modification.

---

## Approach Evaluation

### Approach A — Parser Fork (Rejected)

**Concept:** Fork PostgreSQL, add `CREATE STREAM TABLE` as new grammar in
`gram.y`, ship a modified PostgreSQL binary.

**Why rejected:**
- Must maintain a full PostgreSQL fork, rebasing on every major release
- Cannot use `CREATE EXTENSION` — must replace the user's entire PostgreSQL
- `pg_dump` / `pg_restore` / `psql` all need modifications
- User adoption barrier is extreme (replace your database engine)
- Maintenance cost: `gram.y` changes significantly between PG majors

**Viability:** Only for a PostgreSQL distribution product (like YugabyteDB,
Greenplum). Not viable for a loadable extension.

### Approach B — Table Access Method (Rejected)

**Concept:** Register a custom table AM (`stream_heap`), intercept
`CREATE TABLE ... USING stream_heap WITH (query = '...', schedule = '1m')`.

**Why rejected:**
- Table AM requires implementing 60+ callbacks (scan, insert, delete, vacuum, etc.)
- `CREATE TABLE ... USING` requires explicit column definitions — stream tables
  derive columns from the query, creating redundancy
- No `AS SELECT` support with `USING` clause in PG grammar
- Storing the defining query in `WITH` reloptions is hacky and has length limits
- pg_dump would dump `CREATE TABLE ... USING stream_heap` but not the associated
  metadata (query, schedule)
- Extreme implementation complexity for marginal UX improvement

### Approach C — Foreign Data Wrapper (Rejected)

**Concept:** Register a custom FDW, use `CREATE FOREIGN TABLE ... SERVER
pgstream_server OPTIONS (query '...', schedule '1m')`.

**Why rejected:**
- Foreign tables cannot have indexes — stream tables need `__pgs_row_id` unique index
- Foreign tables cannot have triggers — breaks user-trigger support
- No MVCC snapshot isolation guarantees
- "Foreign table" implies external data, confusing the mental model
- `EXPLAIN` shows "Foreign Scan" instead of "Seq Scan"

### Approach D — ProcessUtility_hook + CREATE MATERIALIZED VIEW (Recommended)

**Concept:** Intercept `CREATE MATERIALIZED VIEW ... WITH (pgstream.stream = true)
AS SELECT ...` via `ProcessUtility_hook`. When the custom option is detected,
route to `create_stream_table_impl()` instead of standard matview creation.

**Why recommended:**
- **Proven pattern** — TimescaleDB uses this exact approach for continuous
  aggregates (`WITH (timescaledb.continuous)`), one of the most widely deployed
  PostgreSQL extensions
- **Native feel** — `CREATE MATERIALIZED VIEW` is the closest standard DDL to
  what a stream table is conceptually (a materialized derived dataset)
- **Query is SQL** — The `AS SELECT ...` clause preserves the query as native
  SQL, enabling IDE syntax highlighting and auto-complete
- **`shared_preload_libraries` already required** — No additional deployment burden
- **Column derivation** — `CREATE MATERIALIZED VIEW ... AS SELECT` naturally
  derives column types from the query, exactly like stream tables
- **Low risk** — The hook only fires for matviews with our specific option;
  all other DDL passes through untouched
- **`DROP` / `REFRESH` integration** — Can intercept `DROP MATERIALIZED VIEW`
  and `REFRESH MATERIALIZED VIEW` for matching stream tables

**Trade-offs:**
- ~1,200–1,800 lines of hook code
- Must track `ProcessUtility_hook` signature changes across PG versions
  (changed in PG14, PG15; stable since)
- pg_dump will dump `CREATE MATERIALIZED VIEW` DDL but our hook must be
  active during restore for it to take effect
- Users may be confused that stream tables appear as "materialized views"
  in some tools

### Approach E — CALL Procedure Syntax (Complementary)

**Concept:** Expose `create_stream_table` as a PostgreSQL procedure
(not just a function), enabling `CALL pgstream.create_stream_table(...)`.

**Why complementary:**
- Trivial to implement (add `#[pg_extern]` with procedure semantics or create
  a wrapper procedure)
- `CALL` reads as a command/action rather than a query
- No `shared_preload_libraries` requirement
- Does not solve the query-as-string problem
- Quick win that can ship independently

---

## Recommended Strategy: Tiered Syntax

### Tier 1: Function API (Existing — No Changes)

```sql
-- Always available, no special requirements
SELECT pgstream.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region',
    '1m', 'DIFFERENTIAL'
);

SELECT pgstream.drop_stream_table('order_totals');
SELECT pgstream.refresh_stream_table('order_totals');
SELECT pgstream.alter_stream_table('order_totals', schedule => '5m');
```

### Tier 1.5: CALL Procedure Syntax (New — Trivial)

```sql
-- Same API but reads as a command, not a query
CALL pgstream.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region',
    '1m', 'DIFFERENTIAL'
);

CALL pgstream.drop_stream_table('order_totals');
CALL pgstream.refresh_stream_table('order_totals');
```

### Tier 2: Native DDL Syntax (New — ProcessUtility_hook)

```sql
-- Create a stream table using familiar matview DDL
CREATE MATERIALIZED VIEW order_totals
WITH (
    pgstream.stream   = true,
    pgstream.schedule = '1m',
    pgstream.mode     = 'DIFFERENTIAL'
)
AS SELECT region, SUM(amount) FROM orders GROUP BY region
WITH NO DATA;

-- Or initialize immediately (WITH DATA is the default):
CREATE MATERIALIZED VIEW order_totals
WITH (pgstream.stream = true)
AS SELECT region, SUM(amount) FROM orders GROUP BY region;

-- Refresh
REFRESH MATERIALIZED VIEW order_totals;

-- Drop
DROP MATERIALIZED VIEW order_totals;
DROP MATERIALIZED VIEW IF EXISTS order_totals CASCADE;

-- Alter (still a function call — no standard ALTER MATERIALIZED VIEW
-- for custom options):
SELECT pgstream.alter_stream_table('order_totals', schedule => '5m');
```

---

## Detailed Design — Tier 2: ProcessUtility_hook

### User-Facing Syntax

#### CREATE

```sql
CREATE MATERIALIZED VIEW [IF NOT EXISTS] [schema.]name
WITH (
    pgstream.stream   = true,         -- required: marks this as a stream table
    pgstream.schedule = '1m',         -- optional: default '1m'
    pgstream.mode     = 'DIFFERENTIAL' -- optional: default 'DIFFERENTIAL'
)
AS select_query
[WITH DATA | WITH NO DATA];
```

**Option semantics:**

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `pgstream.stream` | `bool` | — | **Required.** Must be `true` to trigger stream table creation. |
| `pgstream.schedule` | `text` | `'1m'` | Duration string or cron expression. Set to `'CALCULATED'` for NULL/downstream schedule. |
| `pgstream.mode` | `text` | `'DIFFERENTIAL'` | `'FULL'` or `'DIFFERENTIAL'`. |

- `WITH DATA` (default) → calls `create_stream_table_impl()` with `initialize = true`
- `WITH NO DATA` → calls `create_stream_table_impl()` with `initialize = false`

#### DROP

```sql
DROP MATERIALIZED VIEW [IF EXISTS] [schema.]name [CASCADE | RESTRICT];
```

The hook checks whether the target is a registered stream table (by OID lookup
in `pgstream.pgs_stream_tables`). If so, it routes to `drop_stream_table_impl()`
instead of standard matview drop. If not, it passes through to the standard
handler.

**CASCADE behavior:** If a stream table depends on another stream table being
dropped, `CASCADE` propagates via pg_stream's dependency tracking (same as
`drop_stream_table_impl()` already handles).

#### REFRESH

```sql
REFRESH MATERIALIZED VIEW [CONCURRENTLY] [schema.]name;
```

The hook checks whether the target is a stream table. If so, routes to
`refresh_stream_table_impl()`. The `CONCURRENTLY` keyword is ignored (stream
table refreshes are always concurrent by design — readers see the previous
version until the refresh completes).

#### ALTER

There is no standard `ALTER MATERIALIZED VIEW ... SET (option = value)` syntax
that we can meaningfully intercept for custom options. Alter operations remain
as function calls:

```sql
SELECT pgstream.alter_stream_table('order_totals', schedule => '5m');
SELECT pgstream.alter_stream_table('order_totals', status => 'SUSPENDED');
```

Future PostgreSQL versions may add `ALTER MATERIALIZED VIEW ... SET (reloption)`
support. If so, we can intercept that.

### Hook Registration

```rust
// src/hooks.rs — extended

use std::ffi::c_char;

static mut PREV_PROCESS_UTILITY_HOOK: pg_sys::ProcessUtility_hook_type = None;

/// Register ProcessUtility_hook for native DDL syntax support.
///
/// Called from _PG_init() when loaded via shared_preload_libraries.
/// Chains with any previously installed hook (e.g., pg_stat_statements,
/// TimescaleDB).
pub fn register_process_utility_hook() {
    unsafe {
        PREV_PROCESS_UTILITY_HOOK = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(pg_stream_process_utility);
    }
}
```

In `_PG_init()` (src/lib.rs):

```rust
if in_shared_preload {
    shmem::init_shared_memory();
    scheduler::register_scheduler_worker();

    // NEW: Register ProcessUtility_hook for native DDL syntax
    hooks::register_process_utility_hook();

    log!("pg_stream: initialized (shared_preload_libraries)");
}
```

### CREATE Interception

When `ProcessUtility_hook` fires with a `CreateTableAsStmt` node where
`objtype == OBJECT_MATVIEW`:

```
pg_stream_process_utility()
  ├── Is it CreateTableAsStmt with OBJECT_MATVIEW?
  │     ├── No → pass through to prev hook / standard_ProcessUtility
  │     └── Yes → extract reloptions from IntoClause
  │           ├── Has pgstream.stream = true?
  │           │     ├── No → pass through (normal matview)
  │           │     └── Yes → handle_create_stream_table_ddl()
  │           │           ├── Extract query string from the parse tree
  │           │           ├── Extract pgstream.schedule, pgstream.mode
  │           │           ├── Determine schema.name from IntoClause->rel
  │           │           ├── Determine WITH DATA / WITH NO DATA
  │           │           ├── Call create_stream_table_impl()
  │           │           └── Set QueryCompletion tag
  │           └── Has only non-pgstream options?
  │                 └── Pass through (normal matview with custom storage params)
  └── Is it something else?
        └── Check other interceptions (DROP, REFRESH)
```

**Key implementation detail:** The query text for the defining query must be
extracted from the `CreateTableAsStmt->query` node. Since this is an analyzed
`Query` node (not raw SQL text), we need to either:

1. **Deparse from the parse tree** — Use `pg_sys::nodeToString()` or pgrx
   deparse utilities to reconstruct the SQL from the `SelectStmt` node.
2. **Extract from the raw query string** — The `queryString` parameter to
   the hook contains the full original SQL. Parse it to extract the
   `AS SELECT ...` portion.
3. **Use `pg_get_viewdef`-style deparsing** — Not applicable since the
   matview doesn't exist yet.

Approach (2) is simpler and preserves the user's original formatting.The
`AS` keyword position can be found by walking the query string past the
`WITH (...)` clause.

**Alternatively**, and more robustly: use PostgreSQL's `deparse_query` on
the `SelectStmt` subtree to get a canonical SQL string. This avoids fragile
string parsing and handles edge cases (comments, dollar-quoting, etc.).

### DROP Interception

When `ProcessUtility_hook` fires with a `DropStmt` node where
`removeType == OBJECT_MATVIEW`:

```
pg_stream_process_utility()
  ├── Is it DropStmt with OBJECT_MATVIEW?
  │     └── Yes → for each object in the drop list:
  │           ├── Resolve name to OID
  │           ├── Is OID in pgstream.pgs_stream_tables?
  │           │     ├── No → pass through (normal matview drop)
  │           │     └── Yes → handle_drop_stream_table_ddl()
  │           │           ├── Call drop_stream_table_impl()
  │           │           └── Set QueryCompletion tag
  │           └── Mixed list? (some STs, some normal matviews)
  │                 └── Split: handle STs ourselves, pass rest through
  └── Not a matview drop → pass through
```

**IF EXISTS handling:** If the name doesn't resolve to an OID and `IF EXISTS`
is set in the `DropStmt`, emit a `NOTICE` and continue (matching standard PG
behavior).

### REFRESH Interception

When `ProcessUtility_hook` fires with a `RefreshMatViewStmt` node:

```
pg_stream_process_utility()
  ├── Is it RefreshMatViewStmt?
  │     └── Yes → resolve relation name to OID
  │           ├── Is OID in pgstream.pgs_stream_tables?
  │           │     ├── No → pass through (normal matview refresh)
  │           │     └── Yes → handle_refresh_stream_table_ddl()
  │           │           ├── Call refresh_stream_table_impl()
  │           │           └── Set QueryCompletion tag
  │           └── OID not found → pass through (let standard handler error)
  └── Not a refresh → pass through
```

**CONCURRENTLY:** The `RefreshMatViewStmt` has a `concurrent` field. For
stream tables, we log a `DEBUG1` message that concurrency is implicit and
proceed with the normal refresh.

### ALTER Interception

`ALTER MATERIALIZED VIEW name SET (option = value)` is handled as an
`AlterTableStmt` with `objtype == OBJECT_MATVIEW` and subcommand
`AT_SetRelOptions`. We could intercept this to support:

```sql
ALTER MATERIALIZED VIEW order_totals SET (pgstream.schedule = '5m');
```

However, this interception is complex (must parse subcommands, handle mixed
option sets) and provides marginal UX benefit since `ALTER` is less frequent
than `CREATE`. **Deferred to a future iteration.**

### psql Tab-Completion Compatibility

Since stream tables are created via `CREATE MATERIALIZED VIEW` syntax, they
naturally appear in psql's `\dm` (list materialized views) output. However,
since we intercept creation and create a regular table (not an actual matview),
they will appear in `\dt` (regular tables) instead.

**Mitigation options:**

1. **Actually create a matview** — After our custom processing, also create a
   thin matview entry in `pg_class` with `relkind = 'm'`. This is fragile and
   conflicts with the storage table.

2. **Create a view alias** — Create a regular view with the same name that
   reads from the storage table. The storage table gets an internal name like
   `_pgstream_store_<name>`.

3. **Accept the `\dt` listing** — Stream tables appear as regular tables in
   `\dt`, which is technically accurate (they ARE regular heap tables). The
   `pgstream.stream_tables_info` view provides the authoritative listing.

4. **Register a custom psql command** — Not possible without modifying psql.

**Recommendation:** Option 3 (accept `\dt` listing) for the initial
implementation. The storage table IS a regular table, and `\dt` is correct.
Add `COMMENT ON TABLE` with a descriptive marker so users can identify stream
tables:

```sql
COMMENT ON TABLE schema.name IS 'pgstream: stream table (schedule=1m, mode=DIFFERENTIAL)';
```

### pg_dump / pg_restore Strategy

This is the most significant challenge with the hook-based approach.

#### Problem

`pg_dump` dumps objects based on their `relkind` in `pg_class`. Since stream
tables are stored as regular tables (`relkind = 'r'`), `pg_dump` will emit
`CREATE TABLE` DDL with the full column list — not the `CREATE MATERIALIZED
VIEW ... WITH (pgstream.stream = true) AS SELECT ...` that our hook expects.

The defining query, schedule, and other metadata live in
`pgstream.pgs_stream_tables`, which `pg_dump` dumps as `INSERT` statements
(since it's an extension catalog table in `extension_sql!`).

#### Strategy: Companion Dump/Restore Functions

Provide explicit dump and restore functions:

```sql
-- Generate SQL to recreate all stream tables
SELECT pgstream.generate_dump();

-- Output:
-- SELECT pgstream.create_stream_table('order_totals',
--     'SELECT region, SUM(amount) FROM orders GROUP BY region',
--     '1m', 'DIFFERENTIAL', false);
-- SELECT pgstream.create_stream_table('daily_stats', ...);
```

```sql
-- Restore: run after pg_restore has created the extension and base tables
-- The function reads pgstream.pgs_stream_tables (restored by pg_dump)
-- and recreates the storage tables + CDC + triggers
SELECT pgstream.restore_stream_tables();
```

#### Strategy: Event Trigger on Extension Load

Register an event trigger that fires on `CREATE EXTENSION pg_stream` and
checks if the catalog tables contain orphaned entries (tables exist in catalog
but storage tables are missing). If so, automatically recreate them.

#### pg_dump --section Behavior

| pg_dump section | What it dumps | pg_stream impact |
|----------------|---------------|------------------|
| `pre-data` | Schemas, extensions, types | `CREATE EXTENSION pg_stream` → creates catalog tables |
| `data` | Table contents | Catalog table rows (INSERT INTO pgstream.pgs_stream_tables) + storage table data |
| `post-data` | Indexes, triggers, constraints | Storage table indexes, CDC triggers (but these are extension-managed) |

**Recommended restore workflow:**

```bash
# 1. Restore schema + extension
pg_restore --section=pre-data dump.sql

# 2. Restore data (includes catalog entries + storage table data)
pg_restore --section=data dump.sql

# 3. Rebuild stream tables from catalog (recreates CDC, triggers, indexes)
psql -c "SELECT pgstream.restore_stream_tables();"

# 4. Restore remaining post-data
pg_restore --section=post-data dump.sql
```

### Error Handling

Hook errors must be handled carefully to avoid crashing the backend:

1. **Never `panic!()` in the hook** — Use `pg_guard` + `Result<>` patterns
2. **Failed interception → pass through** — If our hook encounters an error
   during stream table detection (e.g., SPI failure querying catalog), log a
   warning and pass through to the standard handler
3. **Stream table creation errors** — Report via `ereport(ERROR, ...)` as
   usual. The transaction rolls back, leaving no artifacts.
4. **Unknown options** — If `pgstream.stream = true` is present but other
   `pgstream.*` options are unrecognized, raise an error listing valid options

---

## Detailed Design — Tier 1.5: CALL Procedure Syntax

### Implementation

PostgreSQL 11+ supports `CALL procedure(...)` for procedures. Currently
pg_stream exposes functions (callable via `SELECT`). To support `CALL` syntax:

**Option A: Wrapper Procedures (SQL layer)**

```sql
CREATE PROCEDURE pgstream.create_stream_table(
    name          text,
    query         text,
    schedule      text DEFAULT '1m',
    refresh_mode  text DEFAULT 'DIFFERENTIAL',
    initialize    bool DEFAULT true
)
LANGUAGE sql
AS $$
    SELECT pgstream.create_stream_table(name, query, schedule, refresh_mode, initialize);
$$;
```

This approach is straightforward but creates naming conflicts (function and
procedure with same name and signature). PostgreSQL resolves this based on
context (`SELECT` calls the function, `CALL` calls the procedure), but some
tools may be confused.

**Option B: Distinct Procedure Names**

```sql
CALL pgstream.stream_table_create('order_totals', 'SELECT ...', '1m');
CALL pgstream.stream_table_drop('order_totals');
CALL pgstream.stream_table_refresh('order_totals');
```

**Option C: pgrx `#[pg_extern]` with Procedure Support**

pgrx 0.17.x does not yet natively generate `CREATE PROCEDURE`. The wrapper
approach (Option A) via `extension_sql!()` is the most practical.

**Recommendation:** Option A — wrapper procedures with the same name. Add via
`extension_sql!()` in `lib.rs`. This preserves the existing function API and
adds `CALL` as an alias.

### Effort

~30 lines of SQL in `extension_sql!()`. Minimal Rust changes.

---

## Implementation Phases

### Phase 1: Tier 1.5 — CALL Syntax (1–2 days)

**Scope:** Add `CREATE PROCEDURE` wrappers for `create_stream_table`,
`drop_stream_table`, `refresh_stream_table`, and `alter_stream_table`.

**Files changed:**
- `src/lib.rs` — New `extension_sql!()` block with procedure definitions

**Tests:**
- E2E test: create/refresh/drop via `CALL` syntax
- Verify `SELECT` and `CALL` both work for same operations

### Phase 2: Hook Infrastructure (3–5 days)

**Scope:** Register `ProcessUtility_hook` in `_PG_init()`, implement the
dispatch logic, and handle passthrough for non-pgstream DDL.

**Files changed:**
- `src/hooks.rs` — Add `register_process_utility_hook()`,
  `pg_stream_process_utility()` dispatch function, hook chaining
- `src/lib.rs` — Call `hooks::register_process_utility_hook()` in `_PG_init()`

**Tests:**
- Verify hook registration doesn't break existing DDL
- Verify non-pgstream matviews still work
- Verify other extension hooks still chain correctly

### Phase 3: CREATE Interception (5–7 days)

**Scope:** Intercept `CREATE MATERIALIZED VIEW ... WITH (pgstream.stream = true)`
and route to `create_stream_table_impl()`.

**Key tasks:**
1. Parse `CreateTableAsStmt` node to extract reloptions
2. Detect `pgstream.stream = true` in the option list
3. Extract schema, name, query text from the parse tree
4. Extract `pgstream.schedule` and `pgstream.mode` options
5. Determine `WITH DATA` / `WITH NO DATA`
6. Call `create_stream_table_impl()` with extracted parameters
7. Set `QueryCompletion` tag to `"CREATE MATERIALIZED VIEW"`
8. Add `COMMENT ON TABLE` with stream table marker

**Files changed:**
- `src/hooks.rs` — `handle_create_stream_table_ddl()`
- `src/api.rs` — Ensure `create_stream_table_impl()` is `pub(crate)`

**Tests:**
- E2E: Create stream table via matview syntax
- E2E: Verify options parsing (schedule, mode, with data/no data)
- E2E: Verify normal matviews are unaffected
- E2E: Verify error handling (invalid options, bad queries)
- E2E: Verify the resulting table matches function-API-created tables

### Phase 4: DROP + REFRESH Interception (3–4 days)

**Scope:** Intercept `DROP MATERIALIZED VIEW` and `REFRESH MATERIALIZED VIEW`
for registered stream tables.

**Files changed:**
- `src/hooks.rs` — `handle_drop_stream_table_ddl()`,
  `handle_refresh_stream_table_ddl()`

**Tests:**
- E2E: Drop stream table via `DROP MATERIALIZED VIEW`
- E2E: Refresh via `REFRESH MATERIALIZED VIEW`
- E2E: `IF EXISTS` handling
- E2E: `CASCADE` handling
- E2E: Verify normal matviews are unaffected

### Phase 5: pg_dump / Restore Support (3–4 days)

**Scope:** Implement `pgstream.generate_dump()` and
`pgstream.restore_stream_tables()` functions.

**Files changed:**
- `src/api.rs` — New functions `generate_dump()`, `restore_stream_tables()`

**Tests:**
- Integration: Round-trip dump → restore → verify stream tables work
- E2E: Verify restore from pg_dump output

### Phase 6: Documentation + Polish (2–3 days)

**Scope:** Update SQL_REFERENCE.md, GETTING_STARTED.md, FAQ.md. Add migration
guide for existing users.

**Files changed:**
- `docs/SQL_REFERENCE.md` — Document Tier 2 syntax
- `docs/GETTING_STARTED.md` — Show native syntax in examples
- `docs/FAQ.md` — Add "Why CREATE MATERIALIZED VIEW?" Q&A
- `CHANGELOG.md` — Feature announcement

**Total estimated effort: 17–25 days**

---

## Module Layout Changes

```
src/
├── hooks.rs          # Extended: ProcessUtility_hook registration + dispatch
│   ├── (existing)    # DDL event triggers (_on_ddl_end, _on_sql_drop)
│   ├── NEW           # register_process_utility_hook()
│   ├── NEW           # pg_stream_process_utility() — main dispatch
│   ├── NEW           # handle_create_stream_table_ddl()
│   ├── NEW           # handle_drop_stream_table_ddl()
│   ├── NEW           # handle_refresh_stream_table_ddl()
│   └── NEW           # extract_pgstream_options() — reloption parser
├── api.rs            # Unchanged (impl functions already exist)
├── lib.rs            # Extended: hook registration in _PG_init(), CALL procs
└── (all other files unchanged)
```

---

## ADR: Native Syntax Approach

### ADR-012: Native DDL Syntax via ProcessUtility_hook

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Date** | 2026-02-25 |
| **Deciders** | pg_stream core team |
| **Category** | API & Schema Design |

#### Context

Users expect DDL syntax (`CREATE`, `DROP`, `ALTER`) for persistent database
objects. The function-call API works but feels foreign to PostgreSQL users
accustomed to standard DDL. Additionally, embedding the defining query as a
string literal loses IDE support.

#### Decision

Implement a tiered syntax strategy:
- **Tier 1** (existing): Function API — always available, no special requirements
- **Tier 1.5** (new): `CALL` procedure wrappers — trivial addition
- **Tier 2** (new): `CREATE MATERIALIZED VIEW ... WITH (pgstream.stream = true)`
  via `ProcessUtility_hook` — native-feeling DDL when `shared_preload_libraries`
  is configured

#### Options Considered

| Option | Verdict | Reason |
|--------|---------|--------|
| Parser fork | Rejected | Requires PostgreSQL fork, not an extension |
| Table AM | Rejected | 60+ callbacks, column derivation unsupported |
| FDW | Rejected | No indexes, no triggers, wrong semantics |
| **ProcessUtility_hook + matview** | **Selected** | Proven by TimescaleDB, native feel |
| COMMENT abuse | Rejected | Fragile, poor UX |

#### Consequences

**Positive:**
- Users get native-feeling DDL syntax
- Defining query is SQL (not a string literal)
- Follows established pattern (TimescaleDB continuous aggregates)
- No additional deployment burden (shared_preload_libraries already required)

**Negative:**
- ~1,200–1,800 lines of new hook code
- Must track `ProcessUtility_hook` signature across PG versions
- pg_dump requires custom companion functions
- Stream tables appear as regular tables in `\dt`, not `\dm`
- Potential confusion: users may expect full matview semantics (e.g.,
  `REFRESH ... CONCURRENTLY` with different behavior)

---

## Testing Strategy

### Unit Tests (src/hooks.rs `#[cfg(test)]`)

- Option parsing: extract `pgstream.stream`, `pgstream.schedule`, `pgstream.mode`
  from `DefElem` lists
- Schema/name parsing from `RangeVar`
- WITH DATA / WITH NO DATA detection

### E2E Tests (tests/e2e_native_syntax_tests.rs)

| Test | Description |
|------|-------------|
| `test_create_via_matview_syntax` | Basic CREATE MATERIALIZED VIEW with pgstream.stream |
| `test_create_with_all_options` | schedule + mode + WITH NO DATA |
| `test_create_default_options` | Only pgstream.stream = true, all defaults |
| `test_create_schema_qualified` | `CREATE MATERIALIZED VIEW myschema.my_st ...` |
| `test_create_if_not_exists` | IF NOT EXISTS handling |
| `test_drop_via_matview_syntax` | DROP MATERIALIZED VIEW on a stream table |
| `test_drop_if_exists` | DROP MATERIALIZED VIEW IF EXISTS |
| `test_drop_cascade` | CASCADE propagation |
| `test_refresh_via_matview_syntax` | REFRESH MATERIALIZED VIEW on a stream table |
| `test_refresh_concurrently_ignored` | CONCURRENTLY keyword logged but ignored |
| `test_normal_matview_passthrough` | Regular matview without pgstream.stream works normally |
| `test_normal_drop_passthrough` | DROP on non-stream matview works normally |
| `test_mixed_create_function_and_ddl` | Create via function, drop via DDL and vice versa |
| `test_invalid_pgstream_options` | Unknown option → error |
| `test_call_syntax` | CALL pgstream.create_stream_table(...) |
| `test_hook_chaining` | Verify pg_stat_statements still works alongside hook |
| `test_dump_restore_roundtrip` | generate_dump() → restore → verify |

### Property Tests

- For any stream table created via Tier 1 (function), the resulting catalog
  entries and storage table should be identical to one created via Tier 2 (DDL)
  with the same parameters.

---

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| ProcessUtility_hook signature changes in PG 19+ | Medium | Medium | Conditional compilation with `#[cfg(any(...))]` for PG version. Hook signature has been stable since PG 15. |
| Other extension hooks conflict | Low | High | Always chain `prev_ProcessUtility_hook`. Test with pg_stat_statements. Document hook ordering requirements. |
| pg_dump produces incorrect restore SQL | High | Medium | Provide `pgstream.generate_dump()` and `pgstream.restore_stream_tables()`. Document recommended backup workflow. |
| User confusion: "why is my matview in `\dt` not `\dm`?" | Medium | Low | Documentation, COMMENT ON TABLE marker, FAQ entry. |
| Hook code introduces crashes | Low | High | Extensive `#[pg_guard]` usage, never `unwrap()` in hook path, graceful passthrough on errors. |
| Deparse of defining query loses fidelity | Medium | Medium | Use raw query string extraction (approach 2) rather than deparsing from parse tree. Preserve user's original SQL. |
| Extension load order matters | Low | Medium | Document that pg_stream should be listed after pg_stat_statements in `shared_preload_libraries` (or before — test both). |

---

## Comparison with Prior Art

| Feature | pg_stream (proposed) | TimescaleDB | pg_ivm | Citus |
|---------|---------------------|-------------|--------|-------|
| Creation syntax | `CREATE MATVIEW WITH (pgstream.stream)` | `CREATE MATVIEW WITH (timescaledb.continuous)` | `SELECT create_immv(...)` | `SELECT create_distributed_table(...)` |
| Function API | Yes (primary) | Yes (policies) | Yes (primary) | Yes (primary) |
| ProcessUtility_hook | Yes | Yes (extensive) | No | Yes (extensive) |
| pg_dump support | Custom functions | Built-in (complex) | No | Built-in |
| Storage type | Regular table | Hypertable | Regular table | Distributed table |
| REFRESH integration | `REFRESH MATERIALIZED VIEW` | `REFRESH MATERIALIZED VIEW` | `SELECT refresh_immv(...)` | N/A |
| DROP integration | `DROP MATERIALIZED VIEW` | `DROP MATERIALIZED VIEW` | `DROP TABLE` | `SELECT undistribute_table(...)` |

---

## References

1. [Custom SQL Syntax Research Report](../../docs/research/CUSTOM_SQL_SYNTAX.md) —
   Comprehensive analysis of all PostgreSQL extension syntax mechanisms
2. TimescaleDB `process_utility.c` —
   [github.com/timescale/timescaledb](https://github.com/timescale/timescaledb/blob/main/src/process_utility.c)
3. Citus `multi_utility.c` —
   [github.com/citusdata/citus](https://github.com/citusdata/citus/blob/main/src/backend/distributed/commands/multi_utility.c)
4. pg_ivm `createas.c` —
   [github.com/sraoss/pg_ivm](https://github.com/sraoss/pg_ivm/blob/main/createas.c)
5. PostgreSQL `ProcessUtility_hook` —
   `src/backend/tcop/utility.c` in PostgreSQL source
6. PostgreSQL Table AM API —
   [postgresql.org/docs/18/tableam.html](https://www.postgresql.org/docs/18/tableam.html)
7. pgrx Hooks — pgrx documentation on `ProcessUtility_hook` in Rust
8. [ADR Records](../adrs/PLAN_ADRS.md) — ADR-001 (Triggers), ADR-003 (DVM Engine)
