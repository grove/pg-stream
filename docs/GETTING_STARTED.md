# Getting Started with pg_stream

## What is pg_stream?

pg_stream adds **stream tables** to PostgreSQL — tables that are defined by a SQL query and kept automatically up to date as the underlying data changes. Think of them as materialized views that refresh themselves, but smarter: instead of re-running the entire query on every refresh, pg_stream uses **Incremental View Maintenance (IVM)** to process only the rows that changed.

Traditional materialized views force a choice: either re-run the full query (expensive) or accept stale data. pg_stream eliminates this trade-off. When you insert a single row into a million-row table, pg_stream computes the effect of that one row on the query result — it doesn't touch the other 999,999.

### How data flows

The key concept is that **data flows** from your base tables into stream tables automatically:

```
┌──────────────┐     CDC          ┌────────────────┐     Delta Query     ┌──────────────┐
│  Base Tables │ ──triggers──▶    │ Change Buffers │ ────(ΔQ)──────▶     │ Stream Table │
│  (you write) │                  │ (captured rows)│                     │ (auto-updated)│
└──────────────┘                  └────────────────┘                     └──────────────┘
```

1. You write to your base tables normally — `INSERT`, `UPDATE`, `DELETE`
2. Lightweight triggers capture each change into a buffer (no polling, no logical replication)
3. On refresh, pg_stream derives a **delta query** that reads only the buffered changes
4. The delta is merged into the stream table — touched rows are updated, untouched rows are left alone
5. The change buffer is cleaned up

This tutorial walks through a concrete example so you can see this flow in action.

---

## What you'll build

An **employee org-chart** system with two stream tables:

- **`department_tree`** — a recursive CTE that flattens a department hierarchy into paths like `Company > Engineering > Backend`
- **`department_stats`** — a join + aggregation that computes headcount and salary budget per department

By the end you will have:

- Seen how stream tables are created, queried, and refreshed
- Watched INSERTs, UPDATEs, and DELETEs flow through to both stream tables automatically
- Understood what happens under the hood at each step

---

## Prerequisites

- PostgreSQL 18.x with pg_stream installed (see [INSTALL.md](INSTALL.md))
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

Our first stream table flattens the department tree. For every department, it computes the full path from the root and the depth level. This uses `WITH RECURSIVE` — a SQL construct that can't be differentiated algebraically (the recursion depends on itself), but pg_stream handles it using a **recomputation diff** strategy that we'll explain later.

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
               tree.path || '' > '' || d.name AS path,
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

## Step 3: Create the Second Stream Table — Aggregation with Joins

Now create a stream table that joins employees to departments and computes per-department statistics. This demonstrates **algebraic incremental view maintenance** — the most powerful mode, where pg_stream derives a delta formula mathematically from the query structure.

```sql
SELECT pgstream.create_stream_table(
    'department_stats',
    $$
    SELECT
        d.id AS department_id,
        d.name AS department_name,
        COUNT(e.id) AS headcount,
        COALESCE(SUM(e.salary), 0) AS total_salary,
        COALESCE(AVG(e.salary), 0) AS avg_salary
    FROM departments d
    LEFT JOIN employees e ON e.department_id = d.id
    GROUP BY d.id, d.name
    $$,
    '30s',
    'DIFFERENTIAL'
);
```

### What just happened — and why this one is different?

Like before, pg_stream parsed the query, created a storage table, and installed CDC triggers. But this time the query has no recursive CTE, so pg_stream can use **algebraic differentiation**:

1. It decomposed the query into operators: `Scan(departments)` → `LEFT JOIN` → `Scan(employees)` → `Aggregate(GROUP BY + COUNT/SUM/AVG)` → `Project`
2. For each operator, it derived a **differentiation rule** (the math of IVM):
   - `Δ(Scan)` = read only changed rows from the change buffer
   - `Δ(LEFT JOIN)` = join changed rows from one side against the full other side (a "half-join")
   - `Δ(Aggregate)` = for algebraic aggregates like COUNT and SUM, add/subtract the delta without re-scanning the group
3. It composed these rules into a single **delta query** (ΔQ) — a SQL statement that computes the exact effect of the changes, never touching unchanged rows

This means that when you insert one employee, the refresh doesn't re-scan all 7 employees or all 7 departments. It reads one change buffer row, joins it to find the department, and adjusts the count and sum for that one group.

Query it:

```sql
SELECT * FROM department_stats ORDER BY department_name;
```

Expected output:

```
 department_id | department_name | headcount | total_salary | avg_salary
---------------+-----------------+-----------+--------------+------------
             5 | Backend         |         2 |    235000.00 |  117500.00
             1 | Company         |         0 |         0.00 |       0.00
             2 | Engineering     |         0 |         0.00 |       0.00
             6 | Frontend        |         1 |    110000.00 |  110000.00
             4 | Operations      |         1 |    100000.00 |  100000.00
             7 | Platform        |         1 |    130000.00 |  130000.00
             3 | Sales           |         2 |    185000.00 |   92500.00
```

---

## Step 4: Watch Data Flow Through

This is the heart of incremental view maintenance. We'll make four changes to the base tables and observe how each change flows through the pipeline to update the stream tables — processing only the affected rows.

### The data flow pipeline

For every change you'll see these steps happen:

```
  Your SQL statement
       │
       ▼
  CDC trigger fires (same transaction)
       │
       ▼
  Change buffer receives one row
       │
       ▼
  Refresh triggered (manual or scheduled)
       │
       ▼
  Delta query (ΔQ) reads only the buffered changes
       │
       ▼
  MERGE applies the delta to the stream table
       │
       ▼
  Stream table is up to date ✓
```

### 4a: INSERT — Hire a new employee

```sql
INSERT INTO employees (name, department_id, salary) VALUES
    ('Heidi', 6, 105000);  -- New Frontend engineer
```

**What happened behind the scenes:** The `AFTER INSERT` trigger on `employees` fired and wrote one row to the change buffer table `pgstream_changes.changes_<employees_oid>`. This row contains the new values (`name='Heidi'`, `department_id=6`, `salary=105000`), the action type (`I` for insert), and the WAL LSN position at the time of the insert.

The stream tables don't know about this change yet — the change is sitting in the buffer, waiting for the next refresh.

Trigger a refresh (or wait for the 30-second schedule to fire automatically):

```sql
SELECT pgstream.refresh_stream_table('department_stats');
```

**What happened during refresh:**

1. pg_stream checked the change buffer and found 1 new row since the last refresh frontier
2. The **delta query** was executed. For this `LEFT JOIN + GROUP BY` query, the delta does:
   - Read the 1 change buffer row (Heidi, department 6)
   - Join it against `departments` to get the department name ("Frontend")
   - Compute the aggregate delta: COUNT += 1, SUM += 105000
3. The result was a single delta row: `(department_id=6, 'Frontend', +1, +105000, recalculated_avg)`
4. This was **MERGE**d into the `department_stats` storage table — only the Frontend row was touched
5. The change buffer row was cleaned up

Check the result:

```sql
SELECT * FROM department_stats WHERE department_name = 'Frontend';
```

```
 department_id | department_name | headcount | total_salary | avg_salary
---------------+-----------------+-----------+--------------+------------
             6 | Frontend        |         2 |    215000.00 |  107500.00
```

Headcount went from 1 → 2, total salary updated. The 6 other department rows in `department_stats` were **not touched at all** — only the one affected group was updated. That's the power of incremental maintenance.

> **Contrast with a standard materialized view:** `REFRESH MATERIALIZED VIEW` would have re-scanned all 8 employees, re-joined them with all 7 departments, and re-aggregated everything. With pg_stream, the work was proportional to the number of *changes* (1), not the size of the tables.

### 4b: INSERT into a different table — Add a new department

Now let's change the `departments` table instead of `employees`:

```sql
INSERT INTO departments (id, name, parent_id) VALUES
    (8, 'DevOps', 2);  -- New team under Engineering
```

**What happened:** The CDC trigger on *departments* fired (pg_stream installed triggers on both source tables when we created `department_tree`). The change buffer for `departments` now has one new row.

Refresh the recursive tree:

```sql
SELECT pgstream.refresh_stream_table('department_tree');
```

**How the recursive CTE refresh works:** Unlike `department_stats` which uses algebraic differentiation, recursive CTEs use a **recomputation diff** strategy. pg_stream:

1. Re-executes the full `WITH RECURSIVE` query to get the new complete result
2. Compares it against the current `department_tree` storage table using anti-joins
3. Identifies exactly which rows are new (INSERT), which disappeared (DELETE), and which changed (UPDATE)
4. Applies only those differences via MERGE

This is more work than algebraic IVM, but it's the only correct approach for recursive queries — where a single inserted row can cascade through arbitrarily many levels of the recursion.

```sql
SELECT * FROM department_tree WHERE name = 'DevOps';
```

```
 id |  name  | parent_id |           path                | depth
----+--------+-----------+-------------------------------+-------
  8 | DevOps |         2 | Company > Engineering > DevOps |    2
```

The recursive CTE automatically expanded to include the new department at the correct depth and path. One inserted row in `departments` produced one new row in the stream table.

### 4c: UPDATE — Reorganize the company

This is where things get interesting. An UPDATE changes existing data, and the stream tables must reflect both the removal of old values and the addition of new ones.

Suppose Platform is moved from Engineering to Operations:

```sql
UPDATE departments SET parent_id = 4 WHERE id = 7;  -- Platform → Operations
```

**What happened in the change buffer:** The CDC trigger captured both the **old** row values (`parent_id=2`) and the **new** row values (`parent_id=4`). The change buffer stores the before-and-after state so the delta query can compute both what to remove and what to add.

Refresh:

```sql
SELECT pgstream.refresh_stream_table('department_tree');
```

**What happened during refresh:** The recomputation diff re-ran the recursive CTE. Platform's path changed from `Company > Engineering > Platform` to `Company > Operations > Platform`. The MERGE updated that one row in the storage table — all other rows remained untouched.

```sql
SELECT * FROM department_tree WHERE name = 'Platform';
```

```
 id |   name   | parent_id |            path             | depth
----+----------+-----------+-----------------------------+-------
  7 | Platform |         4 | Company > Operations > Platform |   2
```

The path and depth updated correctly — Platform is now under Operations. If Platform had sub-departments, they would have moved too — the recursive CTE recomputation handles arbitrarily deep cascades.

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

## Step 5: Automatic Scheduling — Let Data Flow Hands-Free

In the examples above, we called `refresh_stream_table()` manually. In production, you don't need to do this. pg_stream runs a **background scheduler** that automatically refreshes stream tables when they become stale.

When we created our stream tables with `'30s'`, we told pg_stream: "refresh this table whenever its data is more than 30 seconds out of date." The background worker checks for stale tables every second and triggers refreshes as needed.

Check the current status of your stream tables:

```sql
SELECT * FROM pgstream.pgs_status();
```

This shows each stream table with its:
- **Schedule** — the staleness bound (e.g., 30s) or cron expression
- **Last refresh time** — when data was last synchronized
- **Refresh mode** — DIFFERENTIAL (incremental) or FULL
- **Stale** — whether the table has un-processed changes older than the schedule bound

For detailed performance statistics:

```sql
SELECT * FROM pgstream.pg_stat_stream_tables;
```

This shows refresh counts, timing (average/min/max refresh duration), row counts affected, and error history.

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

### Recomputation Diff (used by `department_tree`)

For recursive CTEs, pg_stream can't differentiate algebraically because the recursion references itself. Instead, it uses a smart **recomputation** strategy:

1. Re-execute the full recursive query → new result set
2. Anti-join the new result against the current storage → find INSERTs (rows in new but not old)
3. Anti-join the current storage against the new result → find DELETEs (rows in old but not new)
4. Join both sides on row ID where values differ → find UPDATEs
5. Apply the minimal set of INSERT/DELETE/UPDATE via MERGE

This is more expensive than algebraic IVM, but still better than a full `TRUNCATE + INSERT` because the MERGE only modifies changed rows — indexes and dead tuples are minimized.

### When to use which?

You don't choose — pg_stream detects the strategy automatically based on the query structure:

| Query Pattern | Strategy | Performance |
|---------------|----------|-------------|
| Scan + Filter + Join + Aggregate | Algebraic | Excellent — O(changes) |
| Non-recursive CTEs | Algebraic | The CTE body is differentiated inline |
| Recursive CTEs (`WITH RECURSIVE`) | Recomputation diff | Good — full re-execute but minimal MERGE |
| Window functions | Partition recompute | Good — only affected partitions recomputed |

---

## Step 7: Clean Up

When you're done experimenting, drop the stream tables:

```sql
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
| **Stream tables** | Tables defined by a query that stay automatically up to date |
| **CDC triggers** | Lightweight change capture — no logical replication, no polling |
| **Algebraic IVM** | Delta queries that process only changed rows (for joins, aggregates, filters) |
| **Recomputation diff** | Smart re-execute + anti-join for recursive CTEs |
| **Data flow** | INSERT/UPDATE/DELETE → trigger → buffer → delta query → MERGE → stream table updated |
| **Scheduling** | Background worker automatically refreshes stale tables within the bound |
| **Monitoring** | `pgs_status()` and `pg_stat_stream_tables` for observability |

The key takeaway: **data flows** from your base tables to your stream tables automatically, and pg_stream does the minimum possible work to keep them in sync.

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
