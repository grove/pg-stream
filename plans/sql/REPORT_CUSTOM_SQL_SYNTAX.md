# Custom SQL Syntax for PostgreSQL Extensions

## Comprehensive Technical Research Report

**Date:** 2026-02-25
**Context:** pg_trickle extension — evaluating approaches to support `CREATE STREAM TABLE` syntax or equivalent native-feeling DDL.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [PostgreSQL Parser Hooks / Utility Hooks](#2-postgresql-parser-hooks--utility-hooks)
3. [The ProcessUtility_hook Approach](#3-the-processutility_hook-approach)
4. [Raw Parser Extension (gram.y)](#4-raw-parser-extension-gramy)
5. [The Utility Command Approach](#5-the-utility-command-approach)
6. [Custom Access Methods (CREATE ACCESS METHOD)](#6-custom-access-methods-create-access-method)
7. [Table Access Method API (PostgreSQL 12+)](#7-table-access-method-api-postgresql-12)
8. [Foreign Data Wrapper Approach](#8-foreign-data-wrapper-approach)
9. [Event Triggers](#9-event-triggers)
10. [TimescaleDB Continuous Aggregates Pattern](#10-timescaledb-continuous-aggregates-pattern)
11. [Citus Distributed DDL Pattern](#11-citus-distributed-ddl-pattern)
12. [PostgreSQL 18 New Features](#12-postgresql-18-new-features)
13. [COMMENT / OPTIONS Abuse Pattern](#13-comment--options-abuse-pattern)
14. [pg_ivm (Incremental View Maintenance) Pattern](#14-pg_ivm-incremental-view-maintenance-pattern)
15. [CREATE TABLE ... USING (Table Access Methods) Deep Dive](#15-create-table--using-table-access-methods-deep-dive)
16. [Comparison Matrix](#16-comparison-matrix)
17. [Recommendations for pg_trickle](#17-recommendations-for-pg_trickle)

---

## 1. Executive Summary

PostgreSQL's parser is **not extensible** — there is no parser hook that allows extensions to add new grammar rules. This is a fundamental design constraint. Every approach to "custom DDL syntax" in extensions falls into one of two categories:

1. **Intercept existing syntax** — Use `ProcessUtility_hook` or event triggers to intercept standard DDL (e.g., `CREATE TABLE`, `CREATE VIEW`) and augment its behavior.
2. **Use a SQL function as the DDL interface** — Define `SELECT my_extension.create_thing(...)` as the user-facing API (this is what pg_trickle currently does).

No production PostgreSQL extension ships truly new SQL grammar without forking the PostgreSQL parser. TimescaleDB, Citus, pg_ivm, and others all work within existing syntax boundaries.

---

## 2. PostgreSQL Parser Hooks / Utility Hooks

### Available Hook Points

PostgreSQL provides several hook function pointers that extensions can override in `_PG_init()`:

| Hook | Header | Purpose |
|------|--------|---------|
| `ProcessUtility_hook` | `tcop/utility.h` | Intercept utility (DDL) statement execution |
| `post_parse_analyze_hook` | `parser/analyze.h` | Inspect/modify the analyzed parse tree after semantic analysis |
| `planner_hook` | `optimizer/planner.h` | Replace or augment the query planner |
| `ExecutorStart_hook` | `executor/executor.h` | Intercept executor startup |
| `ExecutorRun_hook` | `executor/executor.h` | Intercept executor row processing |
| `ExecutorFinish_hook` | `executor/executor.h` | Intercept executor finish |
| `ExecutorEnd_hook` | `executor/executor.h` | Intercept executor cleanup |
| `object_access_hook` | `catalog/objectaccess.h` | Notifications when objects are created/modified/dropped |
| `emit_log_hook` | `utils/elog.h` | Intercept log messages |

### What's Missing: No Parser Hook

**There is no `parser_hook` or `raw_parser_hook`.** The raw parser (`gram.y` → `scan.l` → bison grammar) is compiled into the PostgreSQL server binary. Extensions cannot:

- Add new keywords (e.g., `STREAM`)
- Add new grammar productions (e.g., `CREATE STREAM TABLE`)
- Modify the tokenizer/lexer
- Intercept raw SQL text before parsing

The closest hook is `post_parse_analyze_hook`, which fires **after** the SQL has already been parsed and analyzed. By this point:
- The SQL string has already been tokenized and parsed by gram.y
- A parse tree (`Query` node) has been produced
- If the SQL contains unknown syntax, a `syntax error` has already been raised

### Technical Details of `post_parse_analyze_hook`

```c
/* In src/backend/parser/analyze.c */
typedef void (*post_parse_analyze_hook_type)(ParseState *pstate,
                                             Query *query,
                                             JumbleState *jstate);
post_parse_analyze_hook_type post_parse_analyze_hook = NULL;
```

Extensions can set this in `_PG_init()`:
```c
static post_parse_analyze_hook_type prev_post_parse_analyze_hook = NULL;

void _PG_init(void) {
    prev_post_parse_analyze_hook = post_parse_analyze_hook;
    post_parse_analyze_hook = my_post_parse_analyze;
}
```

**Use cases:** Query rewriting after parsing (e.g., adding security predicates, row-level security), statistics collection, plan caching invalidation. **Not usable for new syntax** because parsing has already completed.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **Impossible** — cannot add new grammar |
| Intercept existing DDL | **Yes** via `ProcessUtility_hook` |
| Modify parsed queries | **Yes** via `post_parse_analyze_hook` |
| Complexity | Low for hooking, but limited in capability |
| PG version | All modern versions (hooks stable since PG 9.x) |
| Maintenance | Very low — hook signatures rarely change |

---

## 3. The ProcessUtility_hook Approach

### How It Works

`ProcessUtility_hook` is the most powerful DDL interception point. It fires for every "utility statement" (DDL, `COPY`, `EXPLAIN`, etc.) **after** parsing but **before** execution.

```c
typedef void (*ProcessUtility_hook_type)(PlannedStmt *pstmt,
                                         const char *queryString,
                                         bool readOnlyTree,
                                         ProcessUtilityContext context,
                                         ParamListInfo params,
                                         QueryEnvironment *queryEnv,
                                         DestReceiver *dest,
                                         QueryCompletion *qc);
```

An extension can:

1. **Inspect the parse tree node** — The `PlannedStmt->utilityStmt` field contains the parsed DDL node (e.g., `CreateStmt`, `AlterTableStmt`, `ViewStmt`).
2. **Modify the parse tree** — Change fields before passing to the standard handler.
3. **Replace execution entirely** — Skip calling the standard handler and do something else.
4. **Post-process** — Call the standard handler first, then do additional work.
5. **Block execution** — Raise an error to prevent the DDL.

### What Extensions Use This

| Extension | What they intercept | Purpose |
|-----------|-------------------|---------|
| **TimescaleDB** | `CREATE TABLE`, `ALTER TABLE`, `DROP TABLE`, `CREATE INDEX`, etc. | Convert regular tables to hypertables, distribute DDL |
| **Citus** | Most DDL statements | Propagate DDL to worker nodes |
| **pg_partman** | `CREATE TABLE`, partition DDL | Auto-manage partitioning |
| **pg_stat_statements** | All utility statements | Track DDL execution statistics |
| **pgAudit** | All utility statements | Audit logging |
| **pg_hint_plan** | — | Uses `post_parse_analyze_hook` instead |
| **sepgsql** | Object creation/modification | Security label enforcement |

### Can It Handle New Syntax?

**No.** It can only intercept DDL that PostgreSQL's parser already understands. You cannot use `ProcessUtility_hook` to handle `CREATE STREAM TABLE` because the parser will reject that syntax before the hook is ever called.

However, it **can** intercept and augment existing syntax:

- `CREATE TABLE ... (some_option)` → Intercept `CreateStmt`, check for special markers, do extra work
- `CREATE VIEW ... WITH (custom_option = true)` → Intercept `ViewStmt`, check `reloptions`
- `CREATE MATERIALIZED VIEW ... WITH (custom = true)` → Same approach

### Pattern: Intercepting CREATE TABLE

```c
static void my_process_utility(PlannedStmt *pstmt, ...) {
    Node *parsetree = pstmt->utilityStmt;

    if (IsA(parsetree, CreateStmt)) {
        CreateStmt *stmt = (CreateStmt *) parsetree;
        // Check for a special reloption or table name pattern
        ListCell *lc;
        foreach(lc, stmt->options) {
            DefElem *opt = (DefElem *) lfirst(lc);
            if (strcmp(opt->defname, "stream") == 0) {
                // This is a stream table! Do custom logic.
                create_stream_table_from_ddl(stmt, queryString);
                return; // Don't call standard handler
            }
        }
    }

    // Pass through to standard handler
    if (prev_ProcessUtility)
        prev_ProcessUtility(pstmt, ...);
    else
        standard_ProcessUtility(pstmt, ...);
}
```

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native `CREATE STREAM TABLE` | **No** — parser rejects unknown syntax |
| `CREATE TABLE ... WITH (stream=true)` | **Yes** — feasible via reloptions |
| Complexity | Medium — must carefully chain with other extensions |
| PG version | All modern versions |
| Maintenance | Low — hook signature changes rarely (changed in PG14, PG15) |
| Risk | Must always chain `prev_ProcessUtility` — misbehaving can break other extensions |

---

## 4. Raw Parser Extension (gram.y)

### How It Works

PostgreSQL's SQL parser is a Bison-generated LALR(1) parser defined in:
- `src/backend/parser/gram.y` — Grammar rules (~18,000 lines)
- `src/backend/parser/scan.l` — Flex lexer (tokenizer)
- `src/include/parser/kwlist.h` — Reserved/unreserved keyword list

To add `CREATE STREAM TABLE`, you would:

1. Add `STREAM` to the keyword list (unreserved or reserved)
2. Add grammar rules to `gram.y`:
   ```yacc
   CreateStreamTableStmt:
       CREATE STREAM TABLE qualified_name '(' OptTableElementList ')'
       OptWith AS SelectStmt
       {
           CreateStreamTableStmt *n = makeNode(CreateStreamTableStmt);
           n->relation = $4;
           n->query = $9;
           /* ... */
           $$ = (Node *) n;
       }
   ;
   ```
3. Add a new `NodeTag` for `CreateStreamTableStmt`
4. Handle it in `ProcessUtility`
5. Rebuild the PostgreSQL server

### Implications

**This requires forking PostgreSQL.** The modified parser is compiled into `postgres` binary. You cannot ship a grammar modification as a loadable extension (`.so`/`.dylib`).

### Who Does This?

- **YugabyteDB** — Fork of PG with custom grammar for distributed features
- **CockroachDB** — Entirely custom parser (Go, not PG's Bison grammar)
- **Amazon Aurora** (partially) — Custom grammar additions for Aurora-specific features
- **Greenplum** — Fork of PG with added grammar for `DISTRIBUTED BY`, `PARTITION BY` etc.
- **ParadeDB** — Fork of PG with some custom syntax additions

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native `CREATE STREAM TABLE` | **Yes** — full parser-level support |
| Complexity | **Very high** — must maintain a PG fork |
| PG version | Tied to a single PG version |
| Maintenance | **Extremely high** — must rebase on every PG release (gram.y changes significantly between major versions) |
| Distribution | Cannot use `CREATE EXTENSION`; must ship entire modified PostgreSQL |
| User adoption | Very low — users must replace their PostgreSQL installation |
| psql autocomplete | Would work with matching psql modifications |
| pg_dump/pg_restore | Broken unless you also modify those tools |

**Verdict:** Not viable for an extension. Only viable for a PostgreSQL fork/distribution.

---

## 5. The Utility Command Approach

### How It Works

Some sources reference a "custom utility command" mechanism. In practice, this does **not** exist as a formal PostgreSQL extension point. What people sometimes mean is one of:

#### 5a. Using DO Blocks as Custom Commands

```sql
DO $$ BEGIN PERFORM pgtrickle.create_stream_table('my_st', 'SELECT ...'); END $$;
```

This is just a wrapped function call — not a real custom command.

#### 5b. Abusing COMMENT or SET for Command Dispatch

Some extensions parse custom commands from strings:

```sql
-- Using SET to pass commands
SET myext.command = 'CREATE STREAM TABLE my_st AS SELECT ...';
SELECT myext.execute_pending_command();
```

Or using `post_parse_analyze_hook` to intercept a specially-formatted query:

```sql
-- Extension intercepts this via post_parse_analyze_hook
SELECT * FROM myext.dispatch('CREATE STREAM TABLE ...');
```

#### 5c. Overloading Existing Syntax

Some extensions overload `SELECT` or `CALL`:

```sql
CALL pgtrickle.create_stream_table('my_st', $$SELECT ...$$);
```

`CALL` was introduced in PostgreSQL 11 for stored procedures. Using it makes the DDL feel more "command-like" than `SELECT function()`.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — still a function call in disguise |
| User experience | Moderate — `CALL` is better than `SELECT` |
| Complexity | Low |
| PG version | PG11+ for `CALL` |
| Maintenance | Very low |

---

## 6. Custom Access Methods (CREATE ACCESS METHOD)

### How It Works

PostgreSQL supports extension-defined access methods (index AMs and table AMs):

```sql
CREATE ACCESS METHOD my_am TYPE TABLE HANDLER my_am_handler;
```

This was introduced in **PostgreSQL 9.6** for index AMs and extended to **table AMs in PostgreSQL 12**. The `CREATE ACCESS METHOD` statement shows PostgreSQL's philosophy: extensions can define new _implementations_ of existing concepts (tables, indexes) but not new _concepts_ (stream tables).

### Table AM vs. Index AM

| Type | Since | Handler Signature | Example |
|------|-------|-------------------|---------|
| Index AM | PG 9.6 | `IndexAmRoutine` with scan/insert/delete callbacks | bloom, brin, GiST |
| Table AM | PG 12 | `TableAmRoutine` with 60+ callbacks | heap (default), columnar (Citus), zedstore (experimental) |

### Can We Use This for Stream Tables?

The table AM API defines how tuples are stored and retrieved, not how tables are created or maintained. A stream table's key features are:

- **Defining query** — Not part of the table AM concept
- **Automatic refresh** — Not part of the table AM concept
- **Change tracking** — Could partially overlap with table AM's tuple modification callbacks
- **Storage** — The actual storage could use heap (default) AM

You could theoretically create a custom table AM that:
1. Uses heap storage underneath
2. Intercepts INSERT/UPDATE/DELETE to maintain change buffers
3. Adds custom metadata

But this would be an extreme abuse of the API. Table AMs are meant for storage engines, not for implementing materialized view semantics.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — `CREATE TABLE ... USING my_am` is the closest |
| Complexity | **Extremely high** — 60+ callbacks to implement |
| Fitness | **Poor** — table AM is about storage, not view maintenance |
| PG version | PG 12+ |
| Maintenance | High — AM API evolves between major versions |

---

## 7. Table Access Method API (PostgreSQL 12+)

### Deep Technical Details

The Table Access Method (AM) API was introduced in PostgreSQL 12 via commit `c2fe139c20` by Andres Freund. It abstracts the storage layer, allowing extensions to replace the default heap storage with custom implementations.

### The `CREATE TABLE ... USING` Syntax

```sql
-- Use default AM (heap)
CREATE TABLE normal_table (id int, data text);

-- Use custom AM
CREATE TABLE my_table (id int, data text) USING my_custom_am;

-- Set default for a database
SET default_table_access_method = 'my_custom_am';
```

### TableAmRoutine Structure

The handler function must return a `TableAmRoutine` struct with callbacks:

```c
typedef struct TableAmRoutine {
    NodeTag type;

    /* Slot callbacks */
    const TupleTableSlotOps *(*slot_callbacks)(Relation rel);

    /* Scan callbacks */
    TableScanDesc (*scan_begin)(Relation rel, Snapshot snap, int nkeys, ...);
    void (*scan_end)(TableScanDesc scan);
    void (*scan_rescan)(TableScanDesc scan, ...);
    bool (*scan_getnextslot)(TableScanDesc scan, ...);

    /* Parallel scan */
    Size (*parallelscan_estimate)(Relation rel);
    Size (*parallelscan_initialize)(Relation rel, ...);
    void (*parallelscan_reinitialize)(Relation rel, ...);

    /* Index fetch */
    IndexFetchTableData *(*index_fetch_begin)(Relation rel);
    void (*index_fetch_reset)(IndexFetchTableData *data);
    void (*index_fetch_end)(IndexFetchTableData *data);
    bool (*index_fetch_tuple)(IndexFetchTableData *data, ...);

    /* Tuple modification */
    void (*tuple_insert)(Relation rel, TupleTableSlot *slot, ...);
    void (*tuple_insert_speculative)(Relation rel, ...);
    void (*tuple_complete_speculative)(Relation rel, ...);
    void (*multi_insert)(Relation rel, TupleTableSlot **slots, int nslots, ...);
    TM_Result (*tuple_delete)(Relation rel, ItemPointer tid, ...);
    TM_Result (*tuple_update)(Relation rel, ItemPointer otid, ...);
    TM_Result (*tuple_lock)(Relation rel, ItemPointer tid, ...);

    /* DDL callbacks */
    void (*relation_set_new_filelocator)(Relation rel, ...);
    void (*relation_nontransactional_truncate)(Relation rel);
    void (*relation_copy_data)(Relation rel, const RelFileLocator *newrlocator);
    void (*relation_copy_for_cluster)(Relation rel, ...);
    void (*relation_vacuum)(Relation rel, VacuumParams *params, ...);
    bool (*scan_analyze_next_block)(TableScanDesc scan, ...);
    bool (*scan_analyze_next_tuple)(TableScanDesc scan, ...);

    /* Planner support */
    void (*relation_estimate_size)(Relation rel, int32 *attr_widths, ...);

    /* ... more callbacks */
} TableAmRoutine;
```

### Hybrid Approach: Table AM + ProcessUtility_hook

A more practical pattern:

1. Register a custom table AM (e.g., `stream_am`) that wraps heap
2. Use `ProcessUtility_hook` to intercept `CREATE TABLE ... USING stream_am`
3. When detected, perform stream table registration (catalog, CDC, etc.)
4. The actual storage uses standard heap via delegation

```sql
-- User writes:
CREATE TABLE order_totals (region text, total numeric)
    USING stream_am
    WITH (query = 'SELECT region, SUM(amount) FROM orders GROUP BY region',
          schedule = '1m',
          refresh_mode = 'DIFFERENTIAL');
```

### Problems with This Approach

1. **Column list is mandatory** — `CREATE TABLE ... USING` requires explicit column definitions. Stream tables should derive columns from the query.
2. **Query in WITH clause** — Storing a full SQL query in `reloptions` is hacky and has length limits.
3. **No AS SELECT** — Table AMs don't support `CREATE TABLE ... AS SELECT` with USING clause in the standard grammar.
4. **VACUUM, ANALYZE complexity** — Must implement or delegate all maintenance callbacks.
5. **pg_dump compatibility** — pg_dump would dump `CREATE TABLE ... USING stream_am` but not the associated metadata (query, schedule, etc.)

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **Partial** — `CREATE TABLE ... USING stream_am` |
| Feels like a stream table | **No** — still looks like a regular table with options |
| Complexity | **Very high** |
| pg_dump | **Broken** — metadata in catalog tables won't be dumped |
| PG version | PG 12+ |
| Maintenance | **High** — table AM API changes between versions |

---

## 8. Foreign Data Wrapper Approach

### How It Works

Foreign Data Wrappers (FDW) allow PostgreSQL to access external data sources via `CREATE FOREIGN TABLE`. An extension can register a custom FDW:

```sql
CREATE EXTENSION pg_trickle;
CREATE SERVER stream_server FOREIGN DATA WRAPPER pgtrickle_fdw;

CREATE FOREIGN TABLE order_totals (region text, total numeric)
    SERVER stream_server
    OPTIONS (
        query 'SELECT region, SUM(amount) FROM orders GROUP BY region',
        schedule '1m',
        refresh_mode 'DIFFERENTIAL'
    );
```

### FDW API

The FDW API provides callbacks for:
- `GetForeignRelSize` — Estimate relation size for planning
- `GetForeignPaths` — Generate access paths
- `GetForeignPlan` — Create a plan node
- `BeginForeignScan` — Start scan
- `IterateForeignScan` — Get next tuple
- `EndForeignScan` — End scan
- `AddForeignUpdatePaths` — Support INSERT/UPDATE/DELETE (optional)

### How It Could Work for Stream Tables

1. Define a custom FDW (`pgtrickle_fdw`)
2. The FDW's scan callbacks read from the underlying storage table
3. `ProcessUtility_hook` intercepts `CREATE FOREIGN TABLE ... SERVER stream_server` to set up CDC, catalog entries, etc.
4. A background worker handles refresh scheduling

### Problems

1. **Foreign tables have restrictions** — Cannot have indexes, constraints, triggers, or participate in inheritance. This severely limits usability.
2. **Query planner limitations** — Foreign tables use a separate planning path with potentially worse plan quality.
3. **No MVCC** — Foreign tables typically don't provide snapshot isolation semantics.
4. **User model confusion** — "Foreign table" implies external data, not a derived view.
5. **EXPLAIN output** — Shows "Foreign Scan" instead of "Seq Scan", confusing users.
6. **pg_dump** — Foreign tables are dumped, but server/FDW setup may not transfer correctly.
7. **Two-step creation** — Requires `CREATE SERVER` before `CREATE FOREIGN TABLE`.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **Partial** — `CREATE FOREIGN TABLE` with options |
| Feels like a stream table | **No** — foreign tables have different semantics |
| Index support | **No** — major limitation |
| Trigger support | **No** — major limitation |
| Complexity | Medium |
| PG version | PG 9.1+ |
| Maintenance | Low — FDW API is very stable |

**Verdict:** Not suitable. The restrictions on foreign tables (no indexes, no triggers) make this impractical for stream tables that need to behave like regular tables.

---

## 9. Event Triggers

### How It Works

Event triggers fire on DDL events at the database level:

```sql
CREATE EVENT TRIGGER my_trigger ON ddl_command_end
    WHEN TAG IN ('CREATE TABLE', 'ALTER TABLE', 'DROP TABLE')
    EXECUTE FUNCTION my_handler();
```

Available events:
- `ddl_command_start` — Before DDL execution (PG 9.3+)
- `ddl_command_end` — After DDL execution (PG 9.3+)
- `sql_drop` — When objects are dropped (PG 9.3+)
- `table_rewrite` — When a table is rewritten (PG 9.5+)

### Inside the Handler

```sql
CREATE FUNCTION my_handler() RETURNS event_trigger AS $$
DECLARE
    obj record;
BEGIN
    FOR obj IN SELECT * FROM pg_event_trigger_ddl_commands()
    LOOP
        -- obj.objid, obj.object_type, obj.command_tag, etc.
        IF obj.command_tag = 'CREATE TABLE' AND obj.object_type = 'table' THEN
            -- Check if this table has a special marker
            -- (e.g., a specific reloption or comment)
        END IF;
    END LOOP;
END;
$$ LANGUAGE plpgsql;
```

### Pattern: CREATE TABLE + Event Trigger

1. User creates a table with a special comment or option:
   ```sql
   CREATE TABLE order_totals (region text, total numeric);
   COMMENT ON TABLE order_totals IS 'pgtrickle:query=SELECT region...;schedule=1m';
   ```
2. Event trigger on `ddl_command_end` fires
3. Handler parses the comment, detects stream table intent
4. Handler registers the stream table in the catalog

### Limitations

1. **Cannot modify the DDL** — Event triggers observe DDL, they can't change what happened. On `ddl_command_end`, the table already exists.
2. **Cannot prevent DDL** — On `ddl_command_start`, you can raise an error to prevent it, but you can't redirect it.
3. **Two-step process** — User must `CREATE TABLE` AND then mark it somehow (comment, option, separate function call).
4. **No custom syntax** — Event triggers watch existing DDL commands.
5. **pg_trickle already uses this** — For DDL tracking on upstream tables (see `hooks.rs`).

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — watches existing DDL only |
| Complexity | Low |
| Can transform DDL | **No** — observe only |
| PG version | PG 9.3+ |
| Maintenance | Very low |
| pg_trickle usage | Already used for upstream DDL tracking |

---

## 10. TimescaleDB Continuous Aggregates Pattern

### How It Works

TimescaleDB continuous aggregates (caggs) demonstrate the **most sophisticated approach** to custom DDL-like syntax in a PostgreSQL extension. Their evolution is instructive.

#### Phase 1: Pure Function API (early versions)

```sql
-- Create a view, then register it
CREATE VIEW daily_temps AS
SELECT time_bucket('1 day', time) AS day, AVG(temp)
FROM conditions GROUP BY 1;

SELECT add_continuous_aggregate_policy('daily_temps', ...);
```

#### Phase 2: CREATE MATERIALIZED VIEW WITH (introduced in TimescaleDB 2.0)

```sql
CREATE MATERIALIZED VIEW daily_temps
WITH (timescaledb.continuous) AS
SELECT time_bucket('1 day', time) AS day, device_id, AVG(temp)
FROM conditions
GROUP BY 1, 2;
```

#### How the Hook Chain Works

TimescaleDB's approach uses **layered hooks**:

1. **`ProcessUtility_hook`** intercepts `CREATE MATERIALIZED VIEW`
2. Checks `reloptions` for `timescaledb.continuous` in the `WithClause`
3. If found:
   - **Does NOT call standard ProcessUtility** for the matview
   - Instead creates a regular hypertable (the materialization)
   - Creates an internal view (the user-facing query interface)
   - Registers refresh policies in the catalog
   - Sets up continuous aggregate metadata
4. For `REFRESH MATERIALIZED VIEW`, intercepts and routes to their refresh engine
5. For `DROP MATERIALIZED VIEW`, intercepts and cleans up all artifacts

#### The Magic: Reloptions as Extension Point

PostgreSQL's `CREATE MATERIALIZED VIEW ... WITH (option = value)` passes options as `DefElem` nodes in the parse tree. The parser treats these as generic key-value pairs — it does NOT validate the option names. This is the key insight: **PostgreSQL's parser accepts arbitrary options in WITH clauses**.

```c
// In ProcessUtility_hook:
if (IsA(parsetree, CreateTableAsStmt)) {
    CreateTableAsStmt *stmt = (CreateTableAsStmt *) parsetree;
    if (stmt->objtype == OBJECT_MATVIEW) {
        // Check for our custom option in stmt->into->options
        bool is_continuous = false;
        ListCell *lc;
        foreach(lc, stmt->into->rel->options) {
            DefElem *opt = (DefElem *) lfirst(lc);
            if (strcmp(opt->defname, "timescaledb.continuous") == 0) {
                is_continuous = true;
                break;
            }
        }
        if (is_continuous) {
            // Handle as continuous aggregate
            return;
        }
    }
}
```

#### Refresh Policies

```sql
-- Add a refresh policy (function call, not DDL)
SELECT add_continuous_aggregate_policy('daily_temps',
    start_offset => INTERVAL '1 month',
    end_offset => INTERVAL '1 day',
    schedule_interval => INTERVAL '1 hour');
```

### What pg_trickle Could Learn

The TimescaleDB pattern for pg_trickle would look like:

```sql
-- Option A: CREATE MATERIALIZED VIEW with custom option
CREATE MATERIALIZED VIEW order_totals
WITH (pgtrickle.stream = true, pgtrickle.schedule = '1m', pgtrickle.mode = 'DIFFERENTIAL')
AS SELECT region, SUM(amount) FROM orders GROUP BY region;

-- Option B: CREATE TABLE with custom option (less natural)
CREATE TABLE order_totals (region text, total numeric)
WITH (pgtrickle.stream = true);
-- Then separately: SELECT pgtrickle.set_query('order_totals', 'SELECT ...');
```

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **Good** — `CREATE MATERIALIZED VIEW ... WITH (pgtrickle.stream)` looks natural |
| User experience | **Very good** — familiar DDL syntax with extension options |
| Complexity | **High** — must implement full ProcessUtility_hook chain |
| pg_dump | **Partial** — matview DDL is dumped, but custom metadata needs `pg_dump` extension or config tables |
| PG version | PG 9.3+ (matviews), PG 12+ (better option handling) |
| Maintenance | Medium — must track changes to matview creation internals |
| Shared preload | **Required** — ProcessUtility_hook needs `shared_preload_libraries` |

---

## 11. Citus Distributed DDL Pattern

### How It Works

Citus (now part of Microsoft) demonstrates another approach to extending DDL behavior:

#### ProcessUtility_hook Chain

Citus has one of the most comprehensive `ProcessUtility_hook` implementations:

```c
void multi_ProcessUtility(PlannedStmt *pstmt, ...) {
    // 1. Classify the DDL
    Node *parsetree = pstmt->utilityStmt;

    // 2. Check if it affects distributed tables
    if (IsA(parsetree, AlterTableStmt)) {
        // Propagate ALTER TABLE to all worker nodes
        PropagateAlterTable((AlterTableStmt *)parsetree, queryString);
    }

    // 3. Call standard handler (or skip for intercepted commands)
    if (prev_ProcessUtility)
        prev_ProcessUtility(pstmt, ...);
    else
        standard_ProcessUtility(pstmt, ...);

    // 4. Post-processing
    if (IsA(parsetree, CreateStmt)) {
        // Check if we should auto-distribute this table
    }
}
```

#### Table Distribution via Function Calls

Citus does NOT add custom DDL syntax. Distribution is done via function calls:

```sql
-- Create a regular table
CREATE TABLE events (id bigint, data jsonb, created_at timestamptz);

-- Distribute it (function call, not DDL)
SELECT create_distributed_table('events', 'id');

-- Or create a reference table
SELECT create_reference_table('lookups');
```

#### Columnar Storage via Table AM

Citus also provides columnar storage as a table AM:

```sql
CREATE TABLE analytics_data (...)
    USING columnar;
```

This uses the table AM API (PostgreSQL 12+) — see Section 7.

### What Citus Teaches Us

- **Function calls for complex operations** — `create_distributed_table()` is analogous to `pgtrickle.create_stream_table()`.
- **ProcessUtility_hook for DDL propagation** — Intercept standard DDL and add behavior.
- **Table AM for storage** — Separate concern from distribution logic.
- **No custom syntax** — Even with Microsoft's resources, Citus doesn't fork the parser.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — uses function calls like pg_trickle |
| Approach validated | **Yes** — Citus is used at massive scale with this pattern |
| Complexity | Medium (function API) to High (ProcessUtility_hook) |
| User adoption | Proven successful |
| Maintenance | Low for function API |

---

## 12. PostgreSQL 18 New Features

### Relevant Extension Points in PG 18

PostgreSQL 18 (released 2025) includes several features relevant to this analysis:

#### 12a. Virtual Generated Columns

PG 18 adds `GENERATED ALWAYS AS (expr) VIRTUAL` columns. Not directly relevant to stream tables, but shows PostgreSQL's willingness to expand `CREATE TABLE` syntax incrementally.

#### 12b. Improved Table AM API

PG 18 refines the table AM API with better TOAST handling and improved parallel scan support. This makes custom table AMs slightly more practical.

#### 12c. Enhanced Event Trigger Information

PG 18 expands `pg_event_trigger_ddl_commands()` with additional metadata fields, making event-trigger-based approaches more capable.

#### 12d. `pg_stat_io` Improvements

Enhanced I/O statistics infrastructure that could benefit monitoring of stream table refresh operations.

#### 12e. No New Parser Extension Points

**PostgreSQL 18 does not add any parser extension mechanism.** The parser remains monolithic and non-extensible. There have been occasional discussions on pgsql-hackers about parser hooks, but no concrete proposals have been accepted.

#### 12f. No Custom DDL Extension Points

No new general-purpose DDL extension points beyond the existing hook system.

### Looking Forward: Discussion on pgsql-hackers

There have been recurring threads on pgsql-hackers about:
- **Extension-defined SQL syntax** — Rejected due to complexity and parser architecture
- **Loadable parser modules** — Theoretical discussions, no implementation
- **Extension catalogs** — Some interest in allowing extensions to register custom catalogs

None of these are implemented in PG 18.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| New syntax extension points | **None** in PG 18 |
| Table AM improvements | **Minor** — slightly easier to implement |
| Event trigger improvements | **Minor** — more metadata available |
| Parser extensibility | **Not planned** for any upcoming PG release |

---

## 13. COMMENT / OPTIONS Abuse Pattern

### How It Works

Several extensions use table comments or reloptions as a "poor man's metadata" to tag tables with custom semantics.

#### Pattern 1: COMMENT-based

```sql
CREATE TABLE order_totals (region text, total numeric);
COMMENT ON TABLE order_totals IS '@pgtrickle {"query": "SELECT ...", "schedule": "1m"}';
```

An event trigger or background worker scans `pg_description` for tables with the `@pgtrickle` prefix and processes them.

#### Pattern 2: Reloptions-based

```sql
CREATE TABLE order_totals (region text, total numeric)
    WITH (fillfactor = 70, pgtrickle.stream = true);
```

**Problem:** PostgreSQL validates reloptions against a known list. You cannot add arbitrary options to `WITH (...)` without registering them. Extensions can register custom reloptions via `add_reloption()` functions, but this is a relatively obscure API.

#### Pattern 3: GUC-based Tagging

```sql
-- Set a GUC that our ProcessUtility_hook reads
SET pgtrickle.next_create_is_stream = true;
SET pgtrickle.stream_query = 'SELECT region, SUM(amount) FROM orders GROUP BY region';

-- Hook intercepts this CREATE TABLE and registers it
CREATE TABLE order_totals (region text, total numeric);

-- Reset
RESET pgtrickle.next_create_is_stream;
```

This is extremely hacky but has been used in practice (some partitioning extensions used similar patterns before native partitioning).

### Who Uses This?

- **pgmemcache** — Uses comments to configure caching behavior
- Some **row-level security** extensions — Comments to define policies
- **pg_partman** — Uses a configuration table (not comments) but similar concept

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — abuses existing mechanisms |
| User experience | **Poor** — fragile, easy to break by editing comments |
| Complexity | Low |
| pg_dump | **COMMENT is dumped** — metadata survives pg_dump/restore |
| Robustness | **Low** — comments can be accidentally changed |
| PG version | All versions |

---

## 14. pg_ivm (Incremental View Maintenance) Pattern

### How It Works

pg_ivm is the most directly comparable extension to pg_trickle. It implements incremental view maintenance for PostgreSQL.

#### API Design

pg_ivm uses a **pure function-call API**:

```sql
-- Create an incrementally maintainable materialized view
SELECT create_immv('order_totals', 'SELECT region, SUM(amount) FROM orders GROUP BY region');

-- Refresh
SELECT refresh_immv('order_totals');

-- Drop
DROP TABLE order_totals;  -- Just drop the underlying table
```

Key function: `create_immv(name, query)` — Creates an "Incrementally Maintainable Materialized View" (IMMV).

#### Internal Implementation

1. `create_immv()` is a SQL function (not a hook)
2. It parses the query, creates a storage table, sets up triggers on source tables
3. IMMVs are stored as regular tables with metadata in a custom catalog (`pg_ivm_immv`)
4. Triggers on source tables automatically update the IMMV on DML

#### No ProcessUtility_hook

pg_ivm does **not** use `ProcessUtility_hook`. It operates entirely through:
- SQL functions (`create_immv`, `refresh_immv`)
- Row-level triggers for automatic maintenance
- A custom catalog table for metadata

#### Why No Custom Syntax?

pg_ivm was developed as a proof-of-concept for PostgreSQL core IVM support. The authors explicitly chose function-call syntax to:
1. Avoid `shared_preload_libraries` requirement (hooks need it)
2. Keep the extension simple and portable
3. Focus on the IVM algorithm, not the user interface

#### Eventually Merged to Core?

There was discussion about upstreaming IVM to PostgreSQL core. If merged, it would get proper syntax (`CREATE INCREMENTAL MATERIALIZED VIEW`). As an extension, it stays with function calls.

### Relevance to pg_trickle

pg_trickle's current API (`pgtrickle.create_stream_table()`) follows the **exact same pattern** as pg_ivm. This is the established approach for IVM extensions.

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **No** — function calls |
| Complexity | **Low** — simple function API |
| shared_preload_libraries | **Not required** for basic function API |
| pg_dump | **No** — function calls are not dumped; must use custom dump/restore |
| User experience | **Moderate** — familiar to pg_ivm users |
| Community acceptance | **Established pattern** for IVM extensions |

---

## 15. CREATE TABLE ... USING (Table Access Methods) Deep Dive

### Full Syntax

```sql
CREATE TABLE tablename (
    column1 datatype,
    column2 datatype,
    ...
) USING access_method_name
  WITH (storage_parameter = value, ...);
```

### How the Parser Handles USING

In `gram.y`:
```yacc
CreateStmt: CREATE OptTemp TABLE ...
    OptTableAccessMethod OptWith ...

OptTableAccessMethod:
    USING name    { $$ = $2; }
    | /* empty */ { $$ = NULL; }
    ;
```

The `USING` clause sets `CreateStmt->accessMethod` to the access method name string.

### How ProcessUtility Handles It

In `createRelation()` (`src/backend/commands/tablecmds.c`):
1. If `accessMethod` is specified, look it up in `pg_am`
2. Verify it's a table AM (not an index AM)
3. Store the AM OID in `pg_class.relam`
4. Use the AM's callbacks for all subsequent operations

### Custom Reloptions with Table AMs

Table AMs can define custom reloptions via:
```c
static relopt_parse_elt stream_relopt_tab[] = {
    {"query", RELOPT_TYPE_STRING, offsetof(StreamOptions, query)},
    {"schedule", RELOPT_TYPE_STRING, offsetof(StreamOptions, schedule)},
    {"refresh_mode", RELOPT_TYPE_STRING, offsetof(StreamOptions, refresh_mode)},
};
```

This would allow:
```sql
CREATE TABLE order_totals (region text, total numeric)
    USING stream_heap
    WITH (query = 'SELECT ...', schedule = '1m', refresh_mode = 'DIFFERENTIAL');
```

### Problems Specific to Stream Tables

1. **Column derivation** — Stream tables derive columns from the query. `CREATE TABLE ... USING` requires explicit column definitions, creating redundancy and potential inconsistency.

2. **No AS SELECT** — You can't combine `USING` with `AS SELECT`:
   ```sql
   -- This does NOT work in PostgreSQL grammar:
   CREATE TABLE order_totals
       USING stream_heap
       AS SELECT region, SUM(amount) FROM orders GROUP BY region;
   ```

3. **Full AM implementation required** — Even if you delegate to heap, you must implement all callbacks and handle edge cases.

4. **VACUUM/ANALYZE** — Must properly delegate to heap for these to work.

5. **Replication** — Logical replication assumes heap tuples; custom AMs may break.

### Hybrid Practical Approach

If pursuing this route:

```sql
-- Step 1: Set default AM
SET default_table_access_method = 'stream_heap';

-- Step 2: Create with query in options
CREATE TABLE order_totals ()
    WITH (pgtrickle.query = 'SELECT region, SUM(amount) FROM orders GROUP BY region',
          pgtrickle.schedule = '1m');

-- ProcessUtility_hook would:
-- 1. Detect USING stream_heap (or detect our custom reloptions)
-- 2. Parse the query from options
-- 3. Derive columns from the query
-- 4. Create the actual table with proper columns using heap AM
-- 5. Register in pgtrickle catalog
-- 6. Set up CDC
```

### Pros/Cons

| Aspect | Assessment |
|--------|-----------|
| Native syntax | **Partial** — `CREATE TABLE ... USING stream_heap WITH (...)` |
| Column derivation | **Not supported** — must specify columns or use hook magic |
| Complexity | **Very high** |
| pg_dump | **Good** — `CREATE TABLE ... USING` is properly dumped |
| PG version | PG 12+ |
| Maintenance | **High** — AM API changes between versions |

---

## 16. Comparison Matrix

| Approach | Native Syntax | Complexity | pg_dump | PG Version | Maintenance | Recommended |
|----------|:------------:|:----------:|:-------:|:----------:|:-----------:|:-----------:|
| Function API (current) | No | Low | No* | Any | Very Low | **Yes** |
| ProcessUtility_hook + MATVIEW WITH | Good | High | Partial | 9.3+ | Medium | **Maybe** |
| Raw parser fork | Perfect | Very High | No | Fork only | Very High | No |
| Table AM USING | Partial | Very High | Yes | 12+ | High | No |
| FDW FOREIGN TABLE | Partial | Medium | Yes | 9.1+ | Low | No |
| Event triggers alone | No | Low | No | 9.3+ | Low | No |
| COMMENT abuse | No | Low | Yes | Any | Low | No |
| GUC + CREATE TABLE hack | No | Medium | Partial | Any | Medium | No |
| TimescaleDB pattern (MATVIEW + WITH) | Good | High | Partial | 9.3+ | Medium | **Best option** |

\* Custom `pg_dump` support can be added via `pg_dump` hook or wrapper script.

---

## 17. Recommendations for pg_trickle

### Current Approach: Function API (Keep and Enhance)

pg_trickle's current approach (`pgtrickle.create_stream_table('name', 'query', ...)`) is:

- **Proven** — Same pattern as pg_ivm, Citus, and many other extensions
- **Simple** — No `shared_preload_libraries` required for basic usage
- **Maintainable** — No hook chains to debug
- **Portable** — Works on any PG version that supports pgrx

**Enhancement opportunities:**
```sql
-- Current
SELECT pgtrickle.create_stream_table('order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region', '1m');

-- Enhanced: CALL syntax for more DDL-like feel (PG 11+)
CALL pgtrickle.create_stream_table('order_totals',
    $$SELECT region, SUM(amount) FROM orders GROUP BY region$$, '1m');
```

### Future Option: TimescaleDB-style Materialized View Integration

If user demand justifies the complexity, pg_trickle could add a **second creation path** via `ProcessUtility_hook`:

```sql
-- New native-feeling syntax (requires shared_preload_libraries)
CREATE MATERIALIZED VIEW order_totals
WITH (pgtrickle.stream = true, pgtrickle.schedule = '1m')
AS SELECT region, SUM(amount) FROM orders GROUP BY region
WITH NO DATA;

-- Original function API still works (no hook needed)
SELECT pgtrickle.create_stream_table('order_totals',
    'SELECT region, SUM(amount) FROM orders GROUP BY region', '1m');
```

**Implementation plan for hook-based approach:**

1. Register `ProcessUtility_hook` in `_PG_init()` (already needed for `shared_preload_libraries`)
2. Intercept `CREATE MATERIALIZED VIEW` → Check for `pgtrickle.stream` option
3. If found: parse options, call `create_stream_table_impl()` internally, create standard storage table instead of matview
4. Intercept `DROP MATERIALIZED VIEW` → Check if target is a stream table → Clean up
5. Intercept `REFRESH MATERIALIZED VIEW` → Route to stream table refresh engine
6. Intercept `ALTER MATERIALIZED VIEW` → Route to stream table alter logic

**Estimated complexity:** ~800-1200 lines of Rust hook code + tests.

### Not Recommended

- **Forking PostgreSQL** for custom grammar — Maintenance cost is prohibitive
- **Table AM approach** — Complexity without proportional benefit
- **FDW approach** — Too many restrictions on foreign tables
- **COMMENT abuse** — Fragile and poor UX

### pg_dump / pg_restore Strategy

Regardless of approach, pg_dump is a challenge. Options:

1. **Custom dump/restore functions** — `pgtrickle.dump_config()` and `pgtrickle.restore_config()` 
2. **Migration script generation** — `pgtrickle.generate_migration()` outputs SQL to recreate all stream tables
3. **Event trigger on restore** — Detect when tables are restored and re-register them
4. **Sidecar file** — Generate a companion SQL file alongside pg_dump

---

## Appendix A: Hook Registration in pgrx (Rust)

For reference, here's how ProcessUtility_hook registration works in pgrx:

```rust
use pgrx::prelude::*;
use pgrx::pg_sys;
use std::ffi::CStr;

static mut PREV_PROCESS_UTILITY_HOOK: pg_sys::ProcessUtility_hook_type = None;

#[pg_guard]
pub extern "C-unwind" fn my_process_utility(
    pstmt: *mut pg_sys::PlannedStmt,
    query_string: *const std::os::raw::c_char,
    read_only_tree: bool,
    context: pg_sys::ProcessUtilityContext,
    params: pg_sys::ParamListInfo,
    query_env: *mut pg_sys::QueryEnvironment,
    dest: *mut pg_sys::DestReceiver,
    qc: *mut pg_sys::QueryCompletion,
) {
    // SAFETY: pstmt is a valid pointer provided by PostgreSQL
    let stmt = unsafe { (*pstmt).utilityStmt };

    // Check if this is a CreateTableAsStmt (materialized view)
    if unsafe { pgrx::is_a(stmt, pg_sys::NodeTag::T_CreateTableAsStmt) } {
        // Check for our custom options...
    }

    // Chain to previous hook or standard handler
    unsafe {
        if let Some(prev) = PREV_PROCESS_UTILITY_HOOK {
            prev(pstmt, query_string, read_only_tree, context,
                 params, query_env, dest, qc);
        } else {
            pg_sys::standard_ProcessUtility(
                pstmt, query_string, read_only_tree, context,
                params, query_env, dest, qc);
        }
    }
}

pub fn register_hooks() {
    unsafe {
        PREV_PROCESS_UTILITY_HOOK = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(my_process_utility);
    }
}
```

---

## Appendix B: Key Source Files in PostgreSQL

| File | Purpose |
|------|---------|
| `src/backend/parser/gram.y` | SQL grammar (~18,000 lines) |
| `src/backend/parser/scan.l` | Lexer/tokenizer |
| `src/include/parser/kwlist.h` | Keyword definitions |
| `src/backend/tcop/utility.c` | `ProcessUtility()` — DDL dispatcher |
| `src/backend/commands/tablecmds.c` | CREATE/ALTER/DROP TABLE implementation |
| `src/backend/commands/createas.c` | CREATE TABLE AS / CREATE MATVIEW AS |
| `src/include/access/tableam.h` | Table Access Method API |
| `src/include/foreign/fdwapi.h` | FDW API |
| `src/backend/commands/event_trigger.c` | Event trigger infrastructure |

---

## Appendix C: References

1. PostgreSQL Documentation — [Table Access Method Interface](https://www.postgresql.org/docs/18/tableam.html)  
2. PostgreSQL Documentation — [Event Triggers](https://www.postgresql.org/docs/18/event-triggers.html)  
3. PostgreSQL Documentation — [Writing A Foreign Data Wrapper](https://www.postgresql.org/docs/18/fdwhandler.html)  
4. TimescaleDB Source — [process_utility.c](https://github.com/timescale/timescaledb/blob/main/src/process_utility.c)  
5. Citus Source — [multi_utility.c](https://github.com/citusdata/citus/blob/main/src/backend/distributed/commands/multi_utility.c)  
6. pg_ivm Source — [createas.c](https://github.com/sraoss/pg_ivm/blob/main/createas.c)  
7. pgrx Documentation — [Hooks](https://github.com/pgcentralfoundation/pgrx/blob/develop/pgrx-examples/custom_types/README.md)  
8. PostgreSQL Wiki — [CustomScanProviders](https://wiki.postgresql.org/wiki/CustomScanProviders)  
