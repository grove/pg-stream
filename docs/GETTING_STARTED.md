# Getting Started with pg_stream

## What is pg_stream?

pg_stream adds **stream tables** to PostgreSQL — tables that are defined by a SQL query and kept automatically up to date as the underlying data changes. Think of them as materialized views that refresh themselves, but smarter: instead of re-running the entire query on every refresh, pg_stream uses **Incremental View Maintenance (IVM)** to process only the rows that changed.

Traditional materialized views force a choice: either re-run the full query (expensive) or accept stale data. pg_stream eliminates this trade-off. When you insert a single row into a million-row table, pg_stream computes the effect of that one row on the query result — it doesn't touch the other 999,999.

### How data flows

The key concept is that **data flows downstream automatically** — from your base tables through any chain of stream tables, without you writing a single line of orchestration code:

```
  You write to base tables
         │
         ▼
  ┌─────────────┐   triggers (or WAL)   ┌─────────────────────┐
  │ Base Tables │ ─────────────────────▶ │   Change Buffers    │
  │ (you write) │                        │ (pgstream_changes.*) │
  └─────────────┘                        └──────────┬──────────┘
                                                     │
                                           delta query (ΔQ) on refresh
                                                     │
                                                     ▼
  ┌──────────────────────────────────────────────────────────────┐
  │  Stream Table A  ◀── depends on base tables                  │
  └──────────────────────────┬───────────────────────────────────┘
                             │  change captured, buffer written
                             ▼
  ┌──────────────────────────────────────────────────────────────┐
  │  Stream Table B  ◀── depends on Stream Table A               │
  └──────────────────────────────────────────────────────────────┘
```

One write to a base table can ripple through an entire DAG of stream tables —
each layer refreshed in the correct topological order, each doing only the work
proportional to what actually changed.

1. You write to your base tables normally — `INSERT`, `UPDATE`, `DELETE`
2. Lightweight `AFTER` row-level triggers capture each change into a buffer, atomically in the same transaction. No polling, no logical replication slots required by default.
3. On each refresh cycle, pg_stream derives a **delta query (ΔQ)** that reads only the buffered changes since the last refresh frontier
4. The delta is merged into the stream table — only the affected rows are written
5. If other stream tables depend on this one, they are scheduled next (topological order)
6. Optionally: once `wal_level = logical` is available and the first refresh succeeds, pg_stream automatically transitions from triggers to **WAL-based CDC** (~2–15 μs write overhead vs triggers). The transition is seamless and transparent.

This tutorial walks through a concrete org-chart example so you can see this flow end to end, including a chain of stream tables that propagates changes automatically.

---

## What you'll build

An **employee org-chart** system with two stream tables:

- **`department_tree`** — a recursive CTE that flattens a department hierarchy into paths like `Company > Engineering > Backend`
- **`department_stats`** — a join + aggregation over `department_tree` (a stream table!) that computes headcount and salary budget, with the full path included
- **`department_report`** — a further aggregation that rolls up stats to top-level departments

The chain `departments` → `department_tree` → `department_stats` → `department_report` demonstrates **automatic downstream propagation**: modify a department name in the base table and all three stream tables update automatically, in the right order, without any manual orchestration.

By the end you will have:

- Seen how stream tables are created, queried, and refreshed
- Watched a single `UPDATE` in a base table cascade through three layers of stream tables automatically
- Understood the two IVM strategies and when each applies

---

## Prerequisites

- PostgreSQL 18.x with pg_stream installed (see [INSTALL.md](../INSTALL.md))
- `shared_preload_libraries = 'pg_stream'` in `postgresql.conf`
- `psql` or any SQL client

---

## Step 1: Create the Base Tables

These are ordinary PostgreSQL tables — pg_stream doesn't require any special column types, annotations, or schema conventions. The only requirement is that tables have a **primary key** (pg_stream uses it internally to track which rows changed).

```sql
-- Department hierarchy (self-referencing tree)
CREATE TABLE departments (
    id         SERIAL PRIMARY KEY,
    name       TEXT NOT NULL,
    parent_id  INT REFERENCES departments(id)
);

-- Employees belong to a department
CREATE TABLE employees (
    id            SERIAL PRIMARY KEY,
    name          TEXT NOT NULL,
    department_id INT NOT NULL REFERENCES departments(id),
    salary        NUMERIC(10,2) NOT NULL
);
```

Now insert some data — a three-level department tree and a handful of employees:

```sql
-- Top-level
INSERT INTO departments (id, name, parent_id) VALUES
    (1, 'Company',     NULL);

-- Second level
INSERT INTO departments (id, name, parent_id) VALUES
    (2, 'Engineering', 1),
    (3, 'Sales',       1),
    (4, 'Operations',  1);

-- Third level (under Engineering)
INSERT INTO departments (id, name, parent_id) VALUES
    (5, 'Backend',     2),
    (6, 'Frontend',    2),
    (7, 'Platform',    2);

-- Employees
INSERT INTO employees (name, department_id, salary) VALUES
    ('Alice',   5, 120000),   -- Backend
    ('Bob',     5, 115000),   -- Backend
    ('Charlie', 6, 110000),   -- Frontend
    ('Diana',   7, 130000),   -- Platform
    ('Eve',     3, 95000),    -- Sales
    ('Frank',   3, 90000),    -- Sales
    ('Grace',   4, 100000);   -- Operations
```

At this point these are plain tables with no triggers, no change tracking, nothing special. The department tree looks like this:

```
Company (1)
├── Engineering (2)
│   ├── Backend (5)     — Alice, Bob
│   ├── Frontend (6)    — Charlie
│   └── Platform (7)    — Diana
├── Sales (3)           — Eve, Frank
└── Operations (4)      — Grace
```

---

## Step 2: Create the First Stream Table — Recursive Hierarchy

Our first stream table flattens the department tree. For every department, it computes the full path from the root and the depth level. This uses `WITH RECURSIVE` — a SQL construct that can't be differentiated with simple algebraic rules (the recursion depends on itself), but pg_stream handles it using **incremental strategies** (semi-naive evaluation for inserts, Delete-and-Rederive for mixed changes) that we'll explain later.

```sql
SELECT pgstream.create_stream_table(
    'department_tree',
    $$
    WITH RECURSIVE tree AS (
        -- Base case: root departments (no parent)
        SELECT id, name, parent_id, name AS path, 0 AS depth
        FROM departments
        WHERE parent_id IS NULL

        UNION ALL

        -- Recursive step: children join back to the tree
        SELECT d.id, d.name, d.parent_id,
               tree.path || ' > ' || d.name AS path,
               tree.depth + 1
        FROM departments d
        JOIN tree ON d.parent_id = tree.id
    )
    SELECT id, name, parent_id, path, depth FROM tree
    $$,
    '30s',
    'DIFFERENTIAL'
);
```

### What just happened?

That single function call did a lot of work atomically (all in one transaction):

1. **Parsed** the defining query into an operator tree — identifying the recursive CTE, the scan on `departments`, the join, the union
2. **Created a storage table** called `department_tree` in the `public` schema — a real PostgreSQL heap table with columns matching the SELECT output, plus internal columns `__pgs_row_id` (a hash used to track individual rows)
3. **Installed CDC triggers** on the `departments` table — lightweight `AFTER INSERT OR UPDATE OR DELETE` row-level triggers that will capture every future change
4. **Created a change buffer table** in the `pgstream_changes` schema — this is where the triggers write captured changes
5. **Ran an initial full refresh** — executed the recursive query against the current data and populated the storage table
6. **Registered the stream table** in pg_stream's catalog with a 30-second refresh schedule

Query it immediately — it's already populated:

```sql
SELECT * FROM department_tree ORDER BY path;
```

Expected output:

```
 id |    name     | parent_id |            path             | depth
----+-------------+-----------+-----------------------------+-------
  1 | Company     |           | Company                     |     0
  2 | Engineering |         1 | Company > Engineering       |     1
  5 | Backend     |         2 | Company > Engineering > Backend  | 2
  6 | Frontend    |         2 | Company > Engineering > Frontend | 2
  7 | Platform    |         2 | Company > Engineering > Platform | 2
  4 | Operations  |         1 | Company > Operations        |     1
  3 | Sales       |         1 | Company > Sales             |     1
```

This is a **real PostgreSQL table** — you can create indexes on it, join it in other queries, reference it in views, or even use it as a source for other stream tables. pg_stream keeps it in sync automatically.

> **Key insight:** The recursive query that computes paths and depths would normally need to be re-run manually (or via `REFRESH MATERIALIZED VIEW`). With pg_stream, it stays fresh — any change to the `departments` table is automatically reflected within the schedule bound (30 seconds here).

---

## Step 3: Chain Stream Tables — Build the Downstream Layers

Now create `department_stats`. The twist: instead of joining directly against `departments`, it joins against `department_tree` — the stream table we just created. This creates a **chain**: changes to `departments` update `department_tree`, whose changes then trigger `department_stats` to update.

This demonstrates how pg_stream builds a **DAG** — a directed acyclic graph of stream tables — and automatically schedules refreshes in topological order.

```sql
SELECT pgstream.create_stream_table(
    'department_stats',
    $$
    SELECT
        t.id          AS department_id,
        t.name        AS department_name,
        t.path        AS full_path,
        t.depth,
        COUNT(e.id)                    AS headcount,
        COALESCE(SUM(e.salary), 0)     AS total_salary,
        COALESCE(AVG(e.salary), 0)     AS avg_salary
    FROM department_tree t
    LEFT JOIN employees e ON e.department_id = t.id
    GROUP BY t.id, t.name, t.path, t.depth
    $$,
    'CALCULATED',       -- inherit schedule from downstream; see explanation below
    'DIFFERENTIAL'
);
```

### What just happened — and why this one is different?

Like before, pg_stream parsed the query, created a storage table, and set up CDC. But `department_stats` depends on `department_tree`, not a base table — so *no new triggers were installed*. Instead, pg_stream registered `department_tree` as an upstream dependency in the DAG.

The schedule is `'CALCULATED'`, which means: "don't give this table its own schedule — inherit the tightest schedule of any downstream table that queries it". Since no other stream table has been created yet, PostgreSQL will prompt a full refresh on demand for now.

The query has no recursive CTE, so pg_stream uses **algebraic differentiation**:

1. Decomposed into operators: `Scan(department_tree)` → `LEFT JOIN` → `Scan(employees)` → `Aggregate(GROUP BY + COUNT/SUM/AVG)` → `Project`
2. Derived a differentiation rule for each:
   - `Δ(Scan)` = read only change buffer rows (not the full table)
   - `Δ(LEFT JOIN)` = join change rows from one side against the full other side
   - `Δ(Aggregate)` = for COUNT/SUM/AVG, add or subtract per group — no rescan needed
3. Composed these into a single **delta query (ΔQ)** that never touches unchanged rows

When one employee is inserted, the refresh reads one change buffer row, joins to find the department, and adjusts only that group's count and sum.

Query it:

```sql
SELECT department_name, full_path, headcount, total_salary
FROM department_stats
ORDER BY full_path;
```

Expected output:

```
 department_name |                 full_path                  | headcount | total_salary
-----------------+--------------------------------------------+-----------+--------------
 Backend         | Company > Engineering > Backend            |         2 |    235000.00
 Frontend        | Company > Engineering > Frontend           |         1 |    110000.00
 Platform        | Company > Engineering > Platform           |         1 |    130000.00
 Engineering     | Company > Engineering                      |         0 |         0.00
 Operations      | Company > Operations                       |         1 |    100000.00
 Sales           | Company > Sales                            |         2 |    185000.00
 Company         | Company                                    |         0 |         0.00
```

Notice that the `full_path` column comes from `department_tree` — this data already went through one layer of incremental maintenance before landing here.

### Add a third layer: `department_report`

Now add a rollup that aggregates `department_stats` by top-level group (depth = 1):

```sql
SELECT pgstream.create_stream_table(
    'department_report',
    $$
    SELECT
        split_part(full_path, ' > ', 2) AS division,
        SUM(headcount)                  AS total_headcount,
        SUM(total_salary)               AS total_payroll
    FROM department_stats
    WHERE depth >= 1
    GROUP BY 1
    $$,
    '30s',            -- this is the only explicit schedule; CALCULATED tables above inherit it
    'DIFFERENTIAL'
);
```

The DAG is now:

```
departments (base)  employees (base)
      │                   │
      ▼                   │
department_tree ──────────┤
   (DIFF, CALCULATED)     │
      │                   ▼
      └──────▶ department_stats
                 (DIFF, CALCULATED)
                      │
                      ▼
               department_report
                  (DIFF, 30s)  ◀── only explicit schedule
```

`department_report` drives the whole pipeline. Because it has a 30-second schedule, pg_stream automatically propagates that cadence upstream: `department_stats` and `department_tree` will also be refreshed within 30 seconds of a base table change, in topological order, with no manual configuration.

Query the report:

```sql
SELECT * FROM department_report ORDER BY division;
```

```
  division   | total_headcount | total_payroll
-------------+-----------------+---------------
 Engineering |               4 |    475000.00
 Operations  |               1 |    100000.00
 Sales       |               2 |    185000.00
```

---

## Step 4: Watch a Change Cascade Through All Three Layers

This is the heart of pg_stream. We'll make four changes to the base tables and watch changes propagate automatically through the three-layer DAG — each layer doing only the minimum work.

### The data flow pipeline (three layers)

```
  Your SQL statement
       │
       ▼
  CDC trigger fires (same transaction)
  Change buffer receives one row
       │
       ▼
  Background scheduler fires (or manual refresh_stream_table)
       │
       ├──▶ [Layer 1] Refresh department_tree
       │         delta query reads change buffer
       │         MERGE touches only affected rows in department_tree
       │         department_tree's own change buffer is updated
       │
       ├──▶ [Layer 2] Refresh department_stats
       │         delta query reads department_tree's change buffer
       │         MERGE touches only affected department groups
       │
       └──▶ [Layer 3] Refresh department_report
                 delta query reads department_stats' change buffer
                 MERGE touches only affected division rows
                 All change buffers cleaned up ✓
```

All three layers run in a single scheduled pass, in topological order.

### 4a: A single INSERT ripples through all three layers

```sql
INSERT INTO employees (name, department_id, salary) VALUES
    ('Heidi', 6, 105000);  -- New Frontend engineer
```

**What happened immediately (in your transaction):** The `AFTER INSERT` trigger on `employees` fired and wrote one row to `pgstream_changes.changes_<employees_oid>`. The row contains the new values, action type `I`, and the LSN at the time of insert. Your transaction committed normally — no blocking.

The stream tables don't know about Heidi yet. The change is in the buffer, waiting for the next refresh.

Refresh the whole pipeline in one call (or wait for the 30-second schedule):

```sql
-- refresh_stream_table cascades to dependent tables automatically
SELECT pgstream.refresh_stream_table('department_report');
```

**What happened across the three layers:**

| Layer | What ran | Rows touched |
|-------|----------|--------------|
| `department_tree` | No change — `employees` is not a source for this ST | 0 |
| `department_stats` | Delta query: read 1 buffer row, join to Frontend, COUNT+1, SUM+105000 | 1 (Frontend group only) |
| `department_report` | Delta query: read 1 change from dept_stats, SUM += 1 headcount, += 105000 | 1 (Engineering row only) |

Check the result:

```sql
SELECT department_name, headcount, total_salary FROM department_stats
WHERE department_name = 'Frontend';
```

```
 department_name | headcount | total_salary
-----------------+-----------+--------------
 Frontend        |         2 |    215000.00
```

The 6 other groups in `department_stats` were **not touched at all**.

> **Contrast with a standard materialized view:** `REFRESH MATERIALIZED VIEW` would re-scan all 8 employees, re-join with all 7 departments, re-aggregate, and update all 7 rows. With pg_stream, the work was proportional to the 1 changed row — across all three layers.

### 4b: A department change cascades through the whole DAG

Now change the `departments` table — the root of the entire chain:

```sql
INSERT INTO departments (id, name, parent_id) VALUES
    (8, 'DevOps', 2);  -- New team under Engineering
```

**What happened:** The CDC trigger on `departments` fired. The change buffer for `departments` has one new row. None of the stream tables know about it yet.

Refresh the whole pipeline:

```sql
SELECT pgstream.refresh_stream_table('department_report');
```

**What happened across all three layers:**

| Layer | What ran | Rows touched |
|-------|----------|--------------|
| `department_tree` | Semi-naive evaluation: base case finds new dept, recursive term computes its path. Result: 1 new row | 1 inserted |
| `department_stats` | Delta query reads new row from dept_tree's change buffer; DevOps has 0 employees so delta is minimal | 1 inserted (headcount=0) |
| `department_report` | Delta on Engineering row: headcount stays the same (DevOps has 0 employees) | 0 effective changes |

**How the recursive CTE refresh works** — unlike `department_stats`, recursive CTEs can't be algebraically differentiated (the recursion references itself). pg_stream uses **incremental fixpoint strategies**:

- **INSERT** → semi-naive evaluation: differentiate the base case, propagate the delta through the recursive term, stopping when no new rows are produced. Only new rows inserted.
- **DELETE or UPDATE** → Delete-and-Rederive (DRed): remove rows derived from deleted facts, re-derive rows that may have alternative derivation paths, handle cascades cleanly.

```sql
SELECT id, name, depth, path FROM department_tree WHERE name = 'DevOps';
```

```
 id |  name  | depth |           path                |
----+--------+-------+-------------------------------+
  8 | DevOps |         2 | Company > Engineering > DevOps |    2
```

The recursive CTE automatically expanded to include the new department at the correct depth and path. One inserted row in `departments` produced one new row in the stream table.

### 4c: UPDATE — A single rename that cascades everywhere

Rename "Engineering" to "R&D":

```sql
UPDATE departments SET name = 'R&D' WHERE id = 2;
```

**What happened in the change buffer:** The CDC trigger captured the **old** row (`name='Engineering'`) and the **new** row (`name='R&D'`). Both old and new values are stored so the delta can compute what to remove and what to add.

Refresh:

```sql
SELECT pgstream.refresh_stream_table('department_report');
```

**What happened:**

| Layer | Work done | Result |
|-------|-----------|--------|
| `department_tree` | DRed strategy: delete rows derived with old name, re-derive with new name. 5 rows updated (Engineering + 4 sub-teams) | Paths now say `Company > R&D > …` |
| `department_stats` | Delta reads 5 changed rows from dept_tree's buffer; updates `full_path` column for those 5 departments | 5 rows updated |
| `department_report` | Division name changed: "Engineering" row replaced by "R&D" row | 1 DELETE + 1 INSERT |

Query to verify the cascade:

```sql
SELECT name, path FROM department_tree WHERE path LIKE '%R&D%' ORDER BY depth;
```

```
  name   |              path
---------+----------------------------------
 R&D     | Company > R&D
 Backend  | Company > R&D > Backend
 DevOps  | Company > R&D > DevOps
 Frontend | Company > R&D > Frontend
 Platform | Company > R&D > Platform
```

One `UPDATE` to a department name flowed through all three layers automatically — updating 5 + 5 + 2 rows across the chain.

### 4d: DELETE — Remove an employee

```sql
DELETE FROM employees WHERE name = 'Bob';
```

**What happened:** The `AFTER DELETE` trigger on `employees` fired, writing a change buffer row with action type `D` and Bob's old values (`department_id=5, salary=115000`). The delta query will use these old values to compute the correct aggregate adjustment — it knows to subtract 115000 from Backend's salary sum and decrement the count.

```sql
SELECT pgstream.refresh_stream_table('department_stats');
SELECT * FROM department_stats WHERE department_name = 'Backend';
```

```
 department_id | department_name | headcount | total_salary | avg_salary
---------------+-----------------+-----------+--------------+------------
             5 | Backend         |         1 |    120000.00 |  120000.00
```

Headcount dropped from 2 → 1 and the salary aggregates updated. Again, only the Backend group was touched — the other 6 department rows were untouched.

---

## Step 5: Automatic Scheduling — Let the DAG Drive Itself

In the examples above, we called `refresh_stream_table()` manually. In production you never need to do this. pg_stream runs a **background scheduler** that automatically refreshes stale tables — in topological order.

### How schedules propagate

We gave `department_report` a `'30s'` schedule and the two upstream tables a `'CALCULATED'` schedule. This is the recommended pattern:

```
 department_tree    (CALCULATED → inherits 30s from downstream)
       │
 department_stats   (CALCULATED → inherits 30s from downstream)
       │
 department_report  (30s — the only explicit schedule)
```

`CALCULATED` means: compute the tightest schedule across all downstream dependents. You declare freshness requirements at the tables your application queries — the system figures out how often each upstream table needs to refresh.

### What the scheduler does every second

1. Queries the catalog for stream tables past their freshness bound
2. Sorts them topologically (upstream first) — `department_tree` refreshes before `department_stats`, which refreshes before `department_report`
3. Runs each refresh (respecting `pg_stream.max_concurrent_refreshes`)
4. Updates the last-refresh frontier

### Monitoring

```sql
-- Current status of all stream tables
SELECT table_name, schedule, last_refresh_at, stale, refresh_mode
FROM pgstream.pgs_status();
```

```
    table_name      | schedule  | last_refresh_at         | stale | refresh_mode
--------------------+-----------+-------------------------+-------+--------------
 department_tree    | 30s       | 2026-02-26 10:30:00.123 | f     | DIFFERENTIAL
 department_stats   | 30s       | 2026-02-26 10:30:00.456 | f     | DIFFERENTIAL
 department_report  | 30s       | 2026-02-26 10:30:00.789 | f     | DIFFERENTIAL
```

```sql
-- Detailed performance stats
SELECT table_name, refresh_count, avg_refresh_ms, rows_affected_last
FROM pgstream.pg_stat_stream_tables;
```

### Optional: WAL-based CDC

By default pg_stream uses triggers. If `wal_level = logical` is configured, set:

```sql
ALTER SYSTEM SET pg_stream.cdc_mode = 'auto';
SELECT pg_reload_conf();
```

pg_stream will automatically transition each stream table from trigger-based to WAL-based capture after the first successful refresh — reducing per-write overhead from ~50–200 μs to ~2–15 μs. The transition is transparent; your queries and the refresh schedule are unaffected.

---

## Step 6: Understanding the Two IVM Strategies

You've now seen both strategies pg_stream uses for incremental view maintenance. Understanding when each applies helps you write efficient stream table queries.

### Algebraic Differentiation (used by `department_stats`)

For queries composed of scans, filters, joins, and algebraic aggregates (COUNT, SUM, AVG), pg_stream can derive the IVM delta **mathematically**. The rules come from the theory of [DBSP (Database Stream Processing)](https://arxiv.org/abs/2203.16684):

| Operator | Delta Rule | Cost |
|----------|-----------|------|
| **Scan** | Read only change buffer rows (not the full table) | O(changes) |
| **Filter (WHERE)** | Apply predicate to change rows | O(changes) |
| **Join** | Join change rows from one side against the full other side | O(changes × lookup) |
| **Aggregate (COUNT/SUM/AVG)** | Add or subtract deltas per group — no rescan | O(affected groups) |
| **Project** | Pass through | O(changes) |

The total cost is proportional to the number of **changes**, not the table size. For a million-row table with 10 changes, the delta query touches ~10 rows.

### Incremental Strategies for Recursive CTEs (used by `department_tree`)

For recursive CTEs, pg_stream can't derive an algebraic delta because the recursion references itself. Instead it uses two complementary strategies, chosen automatically based on what changed:

**Semi-naive evaluation** (for INSERT-only changes):
1. Differentiate the base case — find the new seed rows
2. Propagate the delta through the recursive term, iterating until no new rows are produced
3. The result is only the *new* rows created by the change — not the whole tree

**Delete-and-Rederive (DRed)** (for DELETE or UPDATE):
1. Remove all rows derived from the old fact
2. Re-derive rows that had the old fact as *one of their derivation paths* (they may still be reachable via other paths)
3. Insert the newly derived rows under the new fact

Both strategies are more efficient than full recomputation — they work on the *affected portion of the result set*, not the entire recursive query. The MERGE only modifies rows that actually changed.

### When to use which?

You don't choose — pg_stream detects the strategy automatically based on the query structure:

| Query Pattern | Strategy | Performance |
|---------------|----------|-------------|
| Scan + Filter + Join + algebraic Aggregate (COUNT/SUM/AVG) | Algebraic | Excellent — O(changes) |
| Non-recursive CTEs | Algebraic (inlined) | CTE body is differentiated inline |
| `MIN` / `MAX` aggregates | Semi-algebraic | Uses LEAST/GREATEST merge; per-group rescan only when an extremum is deleted |
| `STRING_AGG`, `ARRAY_AGG`, ordered-set aggregates | Group-rescan | Affected groups fully re-aggregated from source |
| `GROUPING SETS` / `CUBE` / `ROLLUP` | Algebraic (rewritten) | Auto-expanded to `UNION ALL` of `GROUP BY` queries; CUBE capped at 64 branches |
| Recursive CTEs (`WITH RECURSIVE`) INSERT | Semi-naive evaluation | O(new rows derived from the change) |
| Recursive CTEs (`WITH RECURSIVE`) DELETE/UPDATE | Delete-and-Rederive | Re-derives rows with alternative paths; O(affected subgraph) |
| Window functions | Partition recompute | Only affected partitions recomputed |

---

## Step 7: Clean Up

When you're done experimenting, drop the stream tables. Drop dependents before their sources:

```sql
SELECT pgstream.drop_stream_table('department_report');
SELECT pgstream.drop_stream_table('department_stats');
SELECT pgstream.drop_stream_table('department_tree');

DROP TABLE employees;
DROP TABLE departments;
```

`drop_stream_table` atomically removes in a single transaction:
- The storage table (e.g., `public.department_stats`)
- CDC triggers on source tables (removed only if no other stream table references the same source)
- Change buffer tables in `pgstream_changes`
- Catalog entries in `pgstream.pgs_stream_tables`

---

## Summary: What You Learned

| Concept | What you saw |
|---------|-------------|
| **Stream tables** | Tables defined by a SQL query that stay automatically up to date |
| **CDC triggers** | Lightweight change capture in the same transaction — no logical replication or polling required |
| **DAG scheduling** | Stream tables can depend on other stream tables; refreshes run in topological order, schedules propagate upstream via `CALCULATED` mode |
| **Algebraic IVM** | Delta queries that process only changed rows — O(changes) regardless of table size |
| **Semi-naive / DRed** | Incremental strategies for `WITH RECURSIVE` — INSERT uses semi-naive, DELETE/UPDATE uses Delete-and-Rederive |
| **Downstream propagation** | A single base table write cascades through an entire chain of stream tables, automatically, in the right order |
| **Hybrid CDC** | Triggers by default; optional automatic transition to WAL-based capture for lower write-side overhead |
| **Monitoring** | `pgs_status()` and `pg_stat_stream_tables` for freshness, timing, and error history |

The key takeaway: you write to base tables — **pg_stream does the rest**. Data flows downstream automatically, each layer doing the minimum work proportional to what changed, in dependency order.

---

## What's Next?

- **[SQL_REFERENCE.md](SQL_REFERENCE.md)** — Full API reference for all functions, views, and configuration
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — Deep dive into the system architecture and data flow
- **[DVM_OPERATORS.md](DVM_OPERATORS.md)** — How each SQL operator is differentiated for incremental maintenance
- **[CONFIGURATION.md](CONFIGURATION.md)** — GUC variables for tuning schedule, concurrency, and cleanup behavior
- **[What Happens on INSERT](tutorials/WHAT_HAPPENS_ON_INSERT.md)** — Detailed trace of a single INSERT through the entire pipeline
- **[What Happens on UPDATE](tutorials/WHAT_HAPPENS_ON_UPDATE.md)** — How UPDATEs are split into D+I, group key changes, and net-effect computation
- **[What Happens on DELETE](tutorials/WHAT_HAPPENS_ON_DELETE.md)** — Reference counting, group deletion, and INSERT+DELETE cancellation
- **[What Happens on TRUNCATE](tutorials/WHAT_HAPPENS_ON_TRUNCATE.md)** — Why TRUNCATE bypasses triggers and how to recover
