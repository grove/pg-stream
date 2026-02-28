# SQL Reference

Complete reference for all SQL functions, views, and catalog tables provided by pgtrickle.

---

## Functions

### pgtrickle.create_stream_table

Create a new stream table.

```sql
pgtrickle.create_stream_table(
    name          text,
    query         text,
    schedule text      DEFAULT '1m',
    refresh_mode  text      DEFAULT 'DIFFERENTIAL',
    initialize    bool      DEFAULT true
) → void
```

**Parameters:**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `name` | `text` | — | Name of the stream table. May be schema-qualified (`myschema.my_st`). Defaults to `public` schema. |
| `query` | `text` | — | The defining SQL query. Must be a valid SELECT statement using supported operators. |
| `schedule` | `text` | `'1m'` | Refresh schedule as a Prometheus/GNU-style duration string (e.g., `'30s'`, `'5m'`, `'1h'`, `'1h30m'`, `'1d'`) **or** a cron expression (e.g., `'*/5 * * * *'`, `'@hourly'`). Set to `NULL` for CALCULATED mode (inherits schedule from downstream dependents). |
| `refresh_mode` | `text` | `'DIFFERENTIAL'` | `'FULL'` (truncate and reload) or `'DIFFERENTIAL'` (apply delta only). |
| `initialize` | `bool` | `true` | If `true`, populates the table immediately via a full refresh. If `false`, creates the table empty. |

**Duration format:**

| Unit | Suffix | Example |
|---|---|---|
| Seconds | `s` | `'30s'` |
| Minutes | `m` | `'5m'` |
| Hours | `h` | `'2h'` |
| Days | `d` | `'1d'` |
| Weeks | `w` | `'1w'` |
| Compound | — | `'1h30m'`, `'2m30s'` |

**Cron expression format:**

`schedule` also accepts standard cron expressions for time-based scheduling. The scheduler refreshes the stream table when the cron schedule fires, rather than checking staleness.

| Format | Fields | Example | Description |
|---|---|---|---|
| 5-field | min hour dom mon dow | `'*/5 * * * *'` | Every 5 minutes |
| 6-field | sec min hour dom mon dow | `'0 */5 * * * *'` | Every 5 minutes at :00 seconds |
| Alias | — | `'@hourly'` | Every hour |
| Alias | — | `'@daily'` | Every day at midnight |
| Alias | — | `'@weekly'` | Every Sunday at midnight |
| Alias | — | `'@monthly'` | First of every month |
| Weekday range | — | `'0 6 * * 1-5'` | 6 AM on weekdays |

> **Note:** Cron-scheduled stream tables do not participate in CALCULATED schedule resolution. The `stale` column in monitoring views returns `NULL` for cron-scheduled tables.

**Example:**

```sql
-- Duration-based: refresh when data is staler than 2 minutes
SELECT pgtrickle.create_stream_table(
    'order_totals',
    'SELECT region, SUM(amount) AS total FROM orders GROUP BY region',
    '2m',
    'DIFFERENTIAL'
);

-- Cron-based: refresh every hour
SELECT pgtrickle.create_stream_table(
    'hourly_summary',
    'SELECT date_trunc(''hour'', ts), COUNT(*) FROM events GROUP BY 1',
    '@hourly',
    'FULL'
);

-- Cron-based: refresh at 6 AM on weekdays
SELECT pgtrickle.create_stream_table(
    'daily_report',
    'SELECT region, SUM(revenue) AS total FROM sales GROUP BY region',
    '0 6 * * 1-5',
    'FULL'
);
```

**Aggregate Examples:**

All supported aggregate functions work in both FULL and DIFFERENTIAL modes:

```sql
-- Algebraic aggregates (fully differential — no rescan needed)
SELECT pgtrickle.create_stream_table(
    'sales_summary',
    'SELECT region, COUNT(*) AS cnt, SUM(amount) AS total, AVG(amount) AS avg_amount
     FROM orders GROUP BY region',
    '1m',
    'DIFFERENTIAL'
);

-- Semi-algebraic aggregates (MIN/MAX)
SELECT pgtrickle.create_stream_table(
    'salary_ranges',
    'SELECT department, MIN(salary) AS min_sal, MAX(salary) AS max_sal
     FROM employees GROUP BY department',
    '2m',
    'DIFFERENTIAL'
);

-- Group-rescan aggregates (BOOL_AND/OR, STRING_AGG, ARRAY_AGG, JSON_AGG, JSONB_AGG,
--                          BIT_AND, BIT_OR, BIT_XOR, JSON_OBJECT_AGG, JSONB_OBJECT_AGG,
--                          STDDEV, STDDEV_POP, STDDEV_SAMP, VARIANCE, VAR_POP, VAR_SAMP,
--                          MODE, PERCENTILE_CONT, PERCENTILE_DISC,
--                          CORR, COVAR_POP, COVAR_SAMP, REGR_AVGX, REGR_AVGY,
--                          REGR_COUNT, REGR_INTERCEPT, REGR_R2, REGR_SLOPE,
--                          REGR_SXX, REGR_SXY, REGR_SYY, ANY_VALUE)
SELECT pgtrickle.create_stream_table(
    'team_members',
    'SELECT department,
            STRING_AGG(name, '', '' ORDER BY name) AS members,
            ARRAY_AGG(employee_id) AS member_ids,
            BOOL_AND(active) AS all_active,
            JSON_AGG(name) AS members_json
     FROM employees
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- Bitwise aggregates
SELECT pgtrickle.create_stream_table(
    'permission_summary',
    'SELECT department,
            BIT_OR(permissions) AS combined_perms,
            BIT_AND(permissions) AS common_perms,
            BIT_XOR(flags) AS xor_flags
     FROM employees
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- JSON object aggregates
SELECT pgtrickle.create_stream_table(
    'config_map',
    'SELECT department,
            JSON_OBJECT_AGG(setting_name, setting_value) AS settings,
            JSONB_OBJECT_AGG(key, value) AS metadata
     FROM config
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- Statistical aggregates
SELECT pgtrickle.create_stream_table(
    'salary_stats',
    'SELECT department,
            STDDEV_POP(salary) AS sd_pop,
            STDDEV_SAMP(salary) AS sd_samp,
            VAR_POP(salary) AS var_pop,
            VAR_SAMP(salary) AS var_samp
     FROM employees
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- Ordered-set aggregates (MODE, PERCENTILE_CONT, PERCENTILE_DISC)
SELECT pgtrickle.create_stream_table(
    'salary_percentiles',
    'SELECT department,
            MODE() WITHIN GROUP (ORDER BY grade) AS most_common_grade,
            PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY salary) AS median_salary,
            PERCENTILE_DISC(0.9) WITHIN GROUP (ORDER BY salary) AS p90_salary
     FROM employees
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- Regression / correlation aggregates (CORR, COVAR_*, REGR_*)
SELECT pgtrickle.create_stream_table(
    'regression_stats',
    'SELECT department,
            CORR(salary, experience) AS sal_exp_corr,
            COVAR_POP(salary, experience) AS covar_pop,
            COVAR_SAMP(salary, experience) AS covar_samp,
            REGR_SLOPE(salary, experience) AS slope,
            REGR_INTERCEPT(salary, experience) AS intercept,
            REGR_R2(salary, experience) AS r_squared,
            REGR_COUNT(salary, experience) AS regr_n
     FROM employees
     GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- ANY_VALUE aggregate (PostgreSQL 16+)
SELECT pgtrickle.create_stream_table(
    'dept_sample',
    'SELECT department, ANY_VALUE(office_location) AS sample_office
     FROM employees GROUP BY department',
    '1m',
    'DIFFERENTIAL'
);

-- FILTER clause on aggregates
SELECT pgtrickle.create_stream_table(
    'order_metrics',
    'SELECT region,
            COUNT(*) AS total,
            COUNT(*) FILTER (WHERE status = ''active'') AS active_count,
            SUM(amount) FILTER (WHERE status = ''shipped'') AS shipped_total
     FROM orders
     GROUP BY region',
    '1m',
    'DIFFERENTIAL'
);
```

**CTE Examples:**

Non-recursive CTEs are fully supported in both FULL and DIFFERENTIAL modes:

```sql
-- Simple CTE
SELECT pgtrickle.create_stream_table(
    'active_order_totals',
    'WITH active_users AS (
        SELECT id, name FROM users WHERE active = true
    )
    SELECT a.id, a.name, SUM(o.amount) AS total
    FROM active_users a
    JOIN orders o ON o.user_id = a.id
    GROUP BY a.id, a.name',
    '1m',
    'DIFFERENTIAL'
);

-- Chained CTEs (CTE referencing another CTE)
SELECT pgtrickle.create_stream_table(
    'top_regions',
    'WITH regional AS (
        SELECT region, SUM(amount) AS total FROM orders GROUP BY region
    ),
    ranked AS (
        SELECT region, total FROM regional WHERE total > 1000
    )
    SELECT * FROM ranked',
    '2m',
    'DIFFERENTIAL'
);

-- Multi-reference CTE (referenced twice in FROM — shared delta optimization)
SELECT pgtrickle.create_stream_table(
    'self_compare',
    'WITH totals AS (
        SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id
    )
    SELECT t1.user_id, t1.total, t2.total AS next_total
    FROM totals t1
    JOIN totals t2 ON t1.user_id = t2.user_id + 1',
    '1m',
    'DIFFERENTIAL'
);
```

Recursive CTEs work with both FULL and DIFFERENTIAL modes:

```sql
-- Recursive CTE (hierarchy traversal)
SELECT pgtrickle.create_stream_table(
    'category_tree',
    'WITH RECURSIVE cat_tree AS (
        SELECT id, name, parent_id, 0 AS depth
        FROM categories WHERE parent_id IS NULL
        UNION ALL
        SELECT c.id, c.name, c.parent_id, ct.depth + 1
        FROM categories c
        JOIN cat_tree ct ON c.parent_id = ct.id
    )
    SELECT * FROM cat_tree',
    '5m',
    'FULL'  -- FULL mode: standard re-execution
);

-- Recursive CTE with DIFFERENTIAL mode (incremental semi-naive / DRed)
SELECT pgtrickle.create_stream_table(
    'org_chart',
    'WITH RECURSIVE reports AS (
        SELECT id, name, manager_id FROM employees WHERE manager_id IS NULL
        UNION ALL
        SELECT e.id, e.name, e.manager_id
        FROM employees e JOIN reports r ON e.manager_id = r.id
    )
    SELECT * FROM reports',
    '2m',
    'DIFFERENTIAL'  -- Uses semi-naive, DRed, or recomputation (auto-selected)
);
```

> **Non-monotone recursive terms:** If the recursive term contains operators
> like `EXCEPT`, aggregate functions, window functions, `DISTINCT`, `INTERSECT`
> (set), or anti-joins, the system automatically falls back to recomputation
> to guarantee correctness. Semi-naive and DRed strategies require monotone
> recursive terms (JOIN, UNION ALL, filter/project only).

**Set Operation Examples:**

`INTERSECT`, `INTERSECT ALL`, `EXCEPT`, `EXCEPT ALL`, `UNION`, and `UNION ALL` are supported:

```sql
-- INTERSECT: customers who placed orders in BOTH regions
SELECT pgtrickle.create_stream_table(
    'bi_region_customers',
    'SELECT customer_id FROM orders_east
     INTERSECT
     SELECT customer_id FROM orders_west',
    '2m',
    'DIFFERENTIAL'
);

-- INTERSECT ALL: preserves duplicates (bag semantics)
SELECT pgtrickle.create_stream_table(
    'common_items',
    'SELECT item_name FROM warehouse_a
     INTERSECT ALL
     SELECT item_name FROM warehouse_b',
    '1m',
    'DIFFERENTIAL'
);

-- EXCEPT: orders not yet shipped
SELECT pgtrickle.create_stream_table(
    'unshipped_orders',
    'SELECT order_id FROM orders
     EXCEPT
     SELECT order_id FROM shipments',
    '1m',
    'DIFFERENTIAL'
);

-- EXCEPT ALL: preserves duplicate counts (bag subtraction)
SELECT pgtrickle.create_stream_table(
    'excess_inventory',
    'SELECT sku FROM stock_received
     EXCEPT ALL
     SELECT sku FROM stock_shipped',
    '5m',
    'DIFFERENTIAL'
);

-- UNION: deduplicated merge of two sources
SELECT pgtrickle.create_stream_table(
    'all_contacts',
    'SELECT email FROM customers
     UNION
     SELECT email FROM newsletter_subscribers',
    '5m',
    'DIFFERENTIAL'
);
```

**LATERAL Set-Returning Function Examples:**

Set-returning functions (SRFs) in the FROM clause are supported in both FULL and DIFFERENTIAL modes. Common SRFs include `jsonb_array_elements`, `jsonb_each`, `jsonb_each_text`, and `unnest`:

```sql
-- Flatten JSONB arrays into rows
SELECT pgtrickle.create_stream_table(
    'flat_children',
    'SELECT p.id, child.value AS val
     FROM parent_data p,
     jsonb_array_elements(p.data->''children'') AS child',
    '1m',
    'DIFFERENTIAL'
);

-- Expand JSONB key-value pairs (multi-column SRF)
SELECT pgtrickle.create_stream_table(
    'flat_properties',
    'SELECT d.id, kv.key, kv.value
     FROM documents d,
     jsonb_each(d.metadata) AS kv',
    '2m',
    'DIFFERENTIAL'
);

-- Unnest arrays
SELECT pgtrickle.create_stream_table(
    'flat_tags',
    'SELECT t.id, tag.tag
     FROM tagged_items t,
     unnest(t.tags) AS tag(tag)',
    '1m',
    'DIFFERENTIAL'
);

-- SRF with WHERE filter
SELECT pgtrickle.create_stream_table(
    'high_value_items',
    'SELECT p.id, (e.value)::int AS amount
     FROM products p,
     jsonb_array_elements(p.prices) AS e
     WHERE (e.value)::int > 100',
    '5m',
    'DIFFERENTIAL'
);

-- SRF combined with aggregation
SELECT pgtrickle.create_stream_table(
    'element_counts',
    'SELECT a.id, count(*) AS cnt
     FROM arrays a,
     jsonb_array_elements(a.data) AS e
     GROUP BY a.id',
    '1m',
    'FULL'
);
```

**LATERAL Subquery Examples:**

LATERAL subqueries in the FROM clause are supported in both FULL and DIFFERENTIAL modes. Use them for top-N per group, correlated aggregation, and conditional expansion:

```sql
-- Top-N per group: latest item per order
SELECT pgtrickle.create_stream_table(
    'latest_items',
    'SELECT o.id, o.customer, latest.amount
     FROM orders o,
     LATERAL (
         SELECT li.amount
         FROM line_items li
         WHERE li.order_id = o.id
         ORDER BY li.created_at DESC
         LIMIT 1
     ) AS latest',
    '1m',
    'DIFFERENTIAL'
);

-- Correlated aggregate
SELECT pgtrickle.create_stream_table(
    'dept_summaries',
    'SELECT d.id, d.name, stats.total, stats.cnt
     FROM departments d,
     LATERAL (
         SELECT SUM(e.salary) AS total, COUNT(*) AS cnt
         FROM employees e
         WHERE e.dept_id = d.id
     ) AS stats',
    '1m',
    'DIFFERENTIAL'
);

-- LEFT JOIN LATERAL: preserve outer rows with NULLs when subquery returns no rows
SELECT pgtrickle.create_stream_table(
    'dept_stats_all',
    'SELECT d.id, d.name, stats.total
     FROM departments d
     LEFT JOIN LATERAL (
         SELECT SUM(e.salary) AS total
         FROM employees e
         WHERE e.dept_id = d.id
     ) AS stats ON true',
    '1m',
    'DIFFERENTIAL'
);
```

**WHERE Subquery Examples:**

Subqueries in the `WHERE` clause are automatically transformed into semi-join, anti-join, or scalar subquery operators in the DVM operator tree:

```sql
-- EXISTS subquery: customers who have placed orders
SELECT pgtrickle.create_stream_table(
    'active_customers',
    'SELECT c.id, c.name
     FROM customers c
     WHERE EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id)',
    '1m',
    'DIFFERENTIAL'
);

-- NOT EXISTS: customers with no orders
SELECT pgtrickle.create_stream_table(
    'inactive_customers',
    'SELECT c.id, c.name
     FROM customers c
     WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id)',
    '1m',
    'DIFFERENTIAL'
);

-- IN subquery: products that have been ordered
SELECT pgtrickle.create_stream_table(
    'ordered_products',
    'SELECT p.id, p.name
     FROM products p
     WHERE p.id IN (SELECT product_id FROM order_items)',
    '1m',
    'DIFFERENTIAL'
);

-- NOT IN subquery: products never ordered
SELECT pgtrickle.create_stream_table(
    'unordered_products',
    'SELECT p.id, p.name
     FROM products p
     WHERE p.id NOT IN (SELECT product_id FROM order_items)',
    '1m',
    'DIFFERENTIAL'
);

-- Scalar subquery in SELECT list
SELECT pgtrickle.create_stream_table(
    'products_with_max_price',
    'SELECT p.id, p.name, (SELECT max(price) FROM products) AS max_price
     FROM products p',
    '1m',
    'DIFFERENTIAL'
);
```

**Notes:**
- The defining query is parsed into an operator tree and validated for DVM support.
- **Views as sources** — views referenced in the defining query are automatically inlined as subqueries (auto-rewrite pass #0). CDC triggers are created on the underlying base tables. Nested views (view → view → table) are fully expanded. The user's original query is preserved in `original_query` for reinit and introspection. Materialized views are rejected in DIFFERENTIAL mode (use FULL mode or the underlying query directly). Foreign tables are also rejected in DIFFERENTIAL mode.
- CDC triggers and change buffer tables are created automatically for each source table.
- The ST is registered in the dependency DAG; cycles are rejected.
- Non-recursive CTEs are inlined as subqueries during parsing (Tier 1). Multi-reference CTEs share delta computation (Tier 2).
- Recursive CTEs in DIFFERENTIAL mode use three strategies, auto-selected per refresh: **semi-naive evaluation** for INSERT-only changes, **Delete-and-Rederive (DRed)** for mixed changes, and **recomputation fallback** when CTE columns don't match ST storage columns. **Non-monotone recursive terms** (containing EXCEPT, Aggregate, Window, DISTINCT, AntiJoin, or INTERSECT SET) automatically fall back to recomputation to ensure correctness.
- LATERAL SRFs in DIFFERENTIAL mode use row-scoped recomputation: when a source row changes, only the SRF expansions for that row are re-evaluated.
- LATERAL subqueries in DIFFERENTIAL mode also use row-scoped recomputation: when an outer row changes, the correlated subquery is re-executed only for that row.
- WHERE subqueries (`EXISTS`, `IN`, scalar) are parsed into dedicated semi-join, anti-join, and scalar subquery operators with specialized delta computation.
- `ALL (subquery)` is the only subquery form that is currently rejected.
- **ORDER BY** is accepted but silently discarded — row order in the storage table is undefined (consistent with PostgreSQL's `CREATE MATERIALIZED VIEW` behavior). Apply ORDER BY when *querying* the stream table.
- **LIMIT / OFFSET** are rejected — stream tables materialize the full result set. Apply LIMIT when querying the stream table.

---

### pgtrickle.alter_stream_table

Alter properties of an existing stream table.

```sql
pgtrickle.alter_stream_table(
    name          text,
    schedule text      DEFAULT NULL,
    refresh_mode  text      DEFAULT NULL,
    status        text      DEFAULT NULL
) → void
```

**Parameters:**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `name` | `text` | — | Name of the stream table (schema-qualified or unqualified). |
| `schedule` | `text` | `NULL` | New schedule as a duration string (e.g., `'5m'`). Pass `NULL` to leave unchanged. |
| `refresh_mode` | `text` | `NULL` | New refresh mode (`'FULL'` or `'DIFFERENTIAL'`). Pass `NULL` to leave unchanged. |
| `status` | `text` | `NULL` | New status (`'ACTIVE'`, `'SUSPENDED'`). Pass `NULL` to leave unchanged. Resuming resets consecutive errors to 0. |

**Examples:**

```sql
-- Change schedule
SELECT pgtrickle.alter_stream_table('order_totals', schedule => '5m');

-- Switch to full refresh mode
SELECT pgtrickle.alter_stream_table('order_totals', refresh_mode => 'FULL');

-- Suspend a stream table
SELECT pgtrickle.alter_stream_table('order_totals', status => 'SUSPENDED');

-- Resume a suspended stream table
SELECT pgtrickle.resume_stream_table('order_totals');
-- Or via alter_stream_table
SELECT pgtrickle.alter_stream_table('order_totals', status => 'ACTIVE');
```

---

### pgtrickle.drop_stream_table

Drop a stream table, removing the storage table and all catalog entries.

```sql
pgtrickle.drop_stream_table(name text) → void
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `name` | `text` | Name of the stream table to drop. |

**Example:**

```sql
SELECT pgtrickle.drop_stream_table('order_totals');
```

**Notes:**
- Drops the underlying storage table with `CASCADE`.
- Removes all catalog entries (metadata, dependencies, refresh history).
- Cleans up CDC triggers and change buffer tables for source tables that are no longer tracked by any ST.

---

### pgtrickle.resume_stream_table

Resume a suspended stream table, clearing its consecutive error count and
re-enabling automated and manual refreshes.

```sql
pgtrickle.resume_stream_table(name text) → void
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `name` | `text` | Name of the stream table to resume (schema-qualified or unqualified). |

**Example:**

```sql
-- Resume a stream table that was auto-suspended due to repeated errors
SELECT pgtrickle.resume_stream_table('order_totals');
```

**Notes:**
- Errors if the ST is not in `SUSPENDED` state.
- Resets `consecutive_errors` to `0` and sets `status = 'ACTIVE'`.
- Emits a `resumed` event on the `pg_trickle_alert` NOTIFY channel.
- After resuming, the scheduler will include the ST in its next cycle.

---

### pgtrickle.refresh_stream_table

Manually trigger a synchronous refresh of a stream table.

```sql
pgtrickle.refresh_stream_table(name text) → void
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `name` | `text` | Name of the stream table to refresh. |

**Example:**

```sql
SELECT pgtrickle.refresh_stream_table('order_totals');
```

**Notes:**
- Blocked if the ST is `SUSPENDED` — use `pgtrickle.resume_stream_table(name)` first.
- Uses an advisory lock to prevent concurrent refreshes of the same ST.
- For `DIFFERENTIAL` mode, generates and applies a delta query. For `FULL` mode, truncates and reloads.
- Records the refresh in `pgtrickle.pgt_refresh_history`.

---

### pgtrickle.pgt_status

Get the status of all stream tables.

```sql
pgtrickle.pgt_status() → SETOF record(
    name                text,
    status              text,
    refresh_mode        text,
    is_populated        bool,
    consecutive_errors  int,
    schedule            text,
    data_timestamp      timestamptz,
    staleness           interval
)
```

**Example:**

```sql
SELECT * FROM pgtrickle.pgt_status();
```

| name | status | refresh_mode | is_populated | consecutive_errors | schedule | data_timestamp | staleness |
|---|---|---|---|---|---|---|---|
| public.order_totals | ACTIVE | DIFFERENTIAL | true | 0 | 5m | 2026-02-21 12:00:00+00 | 00:02:30 |

---

### pgtrickle.st_refresh_stats

Return per-ST refresh statistics aggregated from the refresh history.

```sql
pgtrickle.st_refresh_stats() → SETOF record(
    pgt_name                text,
    pgt_schema              text,
    status                 text,
    refresh_mode           text,
    is_populated           bool,
    total_refreshes        bigint,
    successful_refreshes   bigint,
    failed_refreshes       bigint,
    total_rows_inserted    bigint,
    total_rows_deleted     bigint,
    avg_duration_ms        float8,
    last_refresh_action    text,
    last_refresh_status    text,
    last_refresh_at        timestamptz,
    staleness_secs       float8,
    stale           bool
)
```

**Example:**

```sql
SELECT pgt_name, status, total_refreshes, avg_duration_ms, stale
FROM pgtrickle.st_refresh_stats();
```

---

### pgtrickle.get_refresh_history

Return refresh history for a specific stream table.

```sql
pgtrickle.get_refresh_history(
    name      text,
    max_rows  int  DEFAULT 20
) → SETOF record(
    refresh_id       bigint,
    data_timestamp   timestamptz,
    start_time       timestamptz,
    end_time         timestamptz,
    action           text,
    status           text,
    rows_inserted    bigint,
    rows_deleted     bigint,
    duration_ms      float8,
    error_message    text
)
```

**Example:**

```sql
SELECT action, status, rows_inserted, duration_ms
FROM pgtrickle.get_refresh_history('order_totals', 5);
```

---

### pgtrickle.get_staleness

Get the current staleness in seconds for a specific stream table.

```sql
pgtrickle.get_staleness(name text) → float8
```

Returns `NULL` if the ST has never been refreshed.

**Example:**

```sql
SELECT pgtrickle.get_staleness('order_totals');
-- Returns: 12.345  (seconds since last refresh)
```

---

### pgtrickle.slot_health

Check replication slot health for all tracked CDC slots.

```sql
pgtrickle.slot_health() → SETOF record(
    slot_name          text,
    source_relid       bigint,
    active             bool,
    retained_wal_bytes bigint,
    wal_status         text
)
```

**Example:**

```sql
SELECT * FROM pgtrickle.slot_health();
```

| slot_name | source_relid | active | retained_wal_bytes | wal_status |
|---|---|---|---|---|
| pg_trickle_slot_16384 | 16384 | false | 1048576 | reserved |

---

### pgtrickle.check_cdc_health

Check CDC health for all tracked source tables. Returns per-source health status including the current CDC mode, replication slot details, estimated lag, and any alerts.

```sql
pgtrickle.check_cdc_health() → SETOF record(
    source_relid   bigint,
    source_table   text,
    cdc_mode       text,
    slot_name      text,
    lag_bytes      bigint,
    confirmed_lsn  text,
    alert          text
)
```

**Columns:**

| Column | Type | Description |
|---|---|---|
| `source_relid` | `bigint` | OID of the tracked source table |
| `source_table` | `text` | Resolved name of the source table (e.g., `public.orders`) |
| `cdc_mode` | `text` | Current CDC mode: `TRIGGER`, `TRANSITIONING`, or `WAL` |
| `slot_name` | `text` | Replication slot name (NULL for TRIGGER mode) |
| `lag_bytes` | `bigint` | Replication slot lag in bytes (NULL for TRIGGER mode) |
| `confirmed_lsn` | `text` | Last confirmed WAL position (NULL for TRIGGER mode) |
| `alert` | `text` | Alert message if unhealthy (e.g., `slot_lag_exceeds_threshold`, `replication_slot_missing`) |

**Example:**

```sql
SELECT * FROM pgtrickle.check_cdc_health();
```

| source_relid | source_table | cdc_mode | slot_name | lag_bytes | confirmed_lsn | alert |
|---|---|---|---|---|---|---|
| 16384 | public.orders | TRIGGER | | | | |
| 16390 | public.events | WAL | pg_trickle_slot_16390 | 524288 | 0/1A8B000 | |

---

### pgtrickle.explain_st

Explain the DVM plan for a stream table's defining query.

```sql
pgtrickle.explain_st(name text) → SETOF record(
    property  text,
    value     text
)
```

**Example:**

```sql
SELECT * FROM pgtrickle.explain_st('order_totals');
```

| property | value |
|---|---|
| Defining Query | SELECT region, SUM(amount) ... |
| Refresh Mode | DIFFERENTIAL |
| Operator Tree | Aggregate → Scan(orders) |
| Source Tables | orders (oid=16384) |
| DVM Supported | Yes |

---

### pgtrickle.pg_trickle_hash

Compute a 64-bit xxHash row ID from a text value.

```sql
pgtrickle.pg_trickle_hash(input text) → bigint
```

Marked `IMMUTABLE, PARALLEL SAFE`.

**Example:**

```sql
SELECT pgtrickle.pg_trickle_hash('some_key');
-- Returns: 1234567890123456789
```

---

### pgtrickle.pg_trickle_hash_multi

Compute a row ID by hashing multiple text values (composite keys).

```sql
pgtrickle.pg_trickle_hash_multi(inputs text[]) → bigint
```

Marked `IMMUTABLE, PARALLEL SAFE`. Uses `\x1E` (record separator) between values and `\x00NULL\x00` for NULL entries.

**Example:**

```sql
SELECT pgtrickle.pg_trickle_hash_multi(ARRAY['key1', 'key2']);
```

---

## Expression Support

pgtrickle's DVM parser supports a wide range of SQL expressions in defining queries. All expressions work in both `FULL` and `DIFFERENTIAL` modes.

### Conditional Expressions

| Expression | Example | Notes |
|---|---|---|
| `CASE WHEN … THEN … ELSE … END` | `CASE WHEN amount > 100 THEN 'high' ELSE 'low' END` | Searched CASE |
| `CASE <expr> WHEN … THEN … END` | `CASE status WHEN 1 THEN 'active' WHEN 2 THEN 'inactive' END` | Simple CASE |
| `COALESCE(a, b, …)` | `COALESCE(phone, email, 'unknown')` | Returns first non-NULL argument |
| `NULLIF(a, b)` | `NULLIF(divisor, 0)` | Returns NULL if `a = b` |
| `GREATEST(a, b, …)` | `GREATEST(score1, score2, score3)` | Returns the largest value |
| `LEAST(a, b, …)` | `LEAST(price, max_price)` | Returns the smallest value |

### Comparison Operators

| Expression | Example | Notes |
|---|---|---|
| `IN (list)` | `category IN ('A', 'B', 'C')` | Also supports `NOT IN` |
| `BETWEEN a AND b` | `price BETWEEN 10 AND 100` | Also supports `NOT BETWEEN` |
| `IS DISTINCT FROM` | `a IS DISTINCT FROM b` | NULL-safe inequality |
| `IS NOT DISTINCT FROM` | `a IS NOT DISTINCT FROM b` | NULL-safe equality |
| `SIMILAR TO` | `name SIMILAR TO '%pattern%'` | SQL regex matching |
| `op ANY(array)` | `id = ANY(ARRAY[1,2,3])` | Array comparison |
| `op ALL(array)` | `score > ALL(ARRAY[50,60])` | Array comparison |

### Boolean Tests

| Expression | Example |
|---|---|
| `IS TRUE` | `active IS TRUE` |
| `IS NOT TRUE` | `flag IS NOT TRUE` |
| `IS FALSE` | `completed IS FALSE` |
| `IS NOT FALSE` | `valid IS NOT FALSE` |
| `IS UNKNOWN` | `result IS UNKNOWN` |
| `IS NOT UNKNOWN` | `flag IS NOT UNKNOWN` |

### SQL Value Functions

| Function | Description |
|---|---|
| `CURRENT_DATE` | Current date |
| `CURRENT_TIME` | Current time with time zone |
| `CURRENT_TIMESTAMP` | Current date and time with time zone |
| `LOCALTIME` | Current time without time zone |
| `LOCALTIMESTAMP` | Current date and time without time zone |
| `CURRENT_ROLE` | Current role name |
| `CURRENT_USER` | Current user name |
| `SESSION_USER` | Session user name |
| `CURRENT_CATALOG` | Current database name |
| `CURRENT_SCHEMA` | Current schema name |

### Array and Row Expressions

| Expression | Example | Notes |
|---|---|---|
| `ARRAY[…]` | `ARRAY[1, 2, 3]` | Array constructor |
| `ROW(…)` | `ROW(a, b, c)` | Row constructor |
| Array subscript | `arr[1]` | Array element access |
| Field access | `(rec).field` | Composite type field access |
| Star indirection | `(data).*` | Expand all fields |

### Subquery Expressions

Subqueries are supported in the `WHERE` clause and `SELECT` list. They are parsed into dedicated DVM operators with specialized delta computation for incremental maintenance.

| Expression | Example | DVM Operator |
|---|---|---|
| `EXISTS (subquery)` | `WHERE EXISTS (SELECT 1 FROM orders WHERE orders.cid = c.id)` | Semi-Join |
| `NOT EXISTS (subquery)` | `WHERE NOT EXISTS (SELECT 1 FROM orders WHERE orders.cid = c.id)` | Anti-Join |
| `IN (subquery)` | `WHERE id IN (SELECT product_id FROM order_items)` | Semi-Join (rewritten as equality) |
| `NOT IN (subquery)` | `WHERE id NOT IN (SELECT product_id FROM order_items)` | Anti-Join |
| Scalar subquery (SELECT) | `SELECT (SELECT max(price) FROM products) AS max_p` | Scalar Subquery |

**Notes:**
- `EXISTS` and `IN (subquery)` in the `WHERE` clause are transformed into semi-join operators. `NOT EXISTS` and `NOT IN (subquery)` become anti-join operators.
- Multiple subqueries in the same `WHERE` clause are supported when combined with `AND`. Subqueries combined with `OR` are also supported — they are automatically rewritten into `UNION` of separate filtered queries.
- Scalar subqueries in the `SELECT` list are supported as long as they return exactly one row and one column.
- `ALL (subquery)` is supported — it is automatically rewritten to an anti-join via `NOT EXISTS` with the negated condition.

### Auto-Rewrite Pipeline

pg_trickle transparently rewrites certain SQL constructs before parsing. These rewrites are applied automatically and require no user action:

| Order | Trigger | Rewrite |
|-------|---------|--------|
| #0 | View references in FROM | Inline view body as subquery |
| #1 | `DISTINCT ON (expr)` | Convert to `ROW_NUMBER() OVER (PARTITION BY expr ORDER BY ...) = 1` subquery |
| #2 | `GROUPING SETS` / `CUBE` / `ROLLUP` | Decompose into `UNION ALL` of separate `GROUP BY` queries |
| #3 | Scalar subquery in `WHERE` | Convert to `CROSS JOIN` with inline view |
| #4 | `EXISTS`/`IN` inside `OR` | Split into `UNION` of separate filtered queries |
| #5 | Multiple `PARTITION BY` clauses | Split into joined subqueries, one per distinct partitioning |

### HAVING Clause

`HAVING` is fully supported. The filter predicate is applied on top of the aggregate delta computation — groups that pass the HAVING condition are included in the stream table.

```sql
SELECT pgtrickle.create_stream_table('big_departments',
  'SELECT department, COUNT(*) AS cnt FROM employees GROUP BY department HAVING COUNT(*) > 10',
  '1m', 'DIFFERENTIAL');
```

### Tables Without Primary Keys (Keyless Tables)

Tables without a primary key can be used as sources. pg_trickle generates a content-based row identity
by hashing all column values using `pg_trickle_hash_multi()`. This allows DIFFERENTIAL mode to work,
though at the cost of being unable to distinguish truly duplicate rows (rows with identical values in all columns).

```sql
-- No primary key — pg_trickle uses content hashing for row identity
CREATE TABLE events (ts TIMESTAMPTZ, payload JSONB);
SELECT pgtrickle.create_stream_table('event_summary',
  'SELECT payload->>''type'' AS event_type, COUNT(*) FROM events GROUP BY 1',
  '1m', 'DIFFERENTIAL');
```

> **Known Limitation — Duplicate Rows in Keyless Tables (G7.1)**
>
> When a keyless table contains **exact duplicate rows** (identical values in every column),
> content-based hashing produces the same `__pgt_row_id` for each copy. Consequences:
>
> - **INSERT** of a duplicate row may appear as a no-op (the hash already exists in the stream table).
> - **DELETE** of one copy may delete all copies (the MERGE matches on `__pgt_row_id`, hitting every duplicate).
> - **Aggregate counts** over keyless tables with duplicates may drift from the true query result.
>
> **Recommendation:** Add a `PRIMARY KEY` or at least a `UNIQUE` constraint to source tables used
> in DIFFERENTIAL mode. This eliminates the ambiguity entirely. If duplicates are expected and
> correctness matters, use `FULL` refresh mode, which always recomputes from scratch.

### Volatile Function Detection

pg_trickle checks all functions and operators in the defining query against `pg_proc.provolatile`:

- **VOLATILE** functions (e.g., `random()`, `now()`, `gen_random_uuid()`) are **rejected** in DIFFERENTIAL mode because they produce different results on each evaluation, breaking delta correctness.
- **VOLATILE operators** — custom operators backed by volatile functions are also detected. The check resolves the operator’s implementation function via `pg_operator.oprcode` and checks its volatility in `pg_proc`.
- **STABLE** functions (e.g., `current_setting()`) produce a **warning** — they are consistent within a single transaction but may differ between refreshes.
- **IMMUTABLE** functions are always safe and produce no warnings.

FULL mode accepts all volatility classes since it re-evaluates the entire query each time.

### COLLATE Expressions

`COLLATE` clauses on expressions are supported:

```sql
SELECT pgtrickle.create_stream_table('sorted_names',
  'SELECT name COLLATE "C" AS c_name FROM users',
  '1m', 'DIFFERENTIAL');
```

### IS JSON Predicate (PostgreSQL 16+)

The `IS JSON` predicate validates whether a value is valid JSON. All variants are supported:

```sql
-- Filter rows with valid JSON
SELECT pgtrickle.create_stream_table('valid_json_events',
  'SELECT id, payload FROM events WHERE payload::text IS JSON',
  '1m', 'DIFFERENTIAL');

-- Type-specific checks
SELECT pgtrickle.create_stream_table('json_objects_only',
  'SELECT id, data IS JSON OBJECT AS is_obj,
          data IS JSON ARRAY AS is_arr,
          data IS JSON SCALAR AS is_scalar
   FROM json_data',
  '1m', 'FULL');
```

Supported variants: `IS JSON`, `IS JSON OBJECT`, `IS JSON ARRAY`, `IS JSON SCALAR`, `IS NOT JSON` (all forms), `WITH UNIQUE KEYS`.

### SQL/JSON Constructors (PostgreSQL 16+)

SQL-standard JSON constructor functions are supported in both FULL and DIFFERENTIAL modes:

```sql
-- JSON_OBJECT: construct a JSON object from key-value pairs
SELECT pgtrickle.create_stream_table('user_json',
  'SELECT id, JSON_OBJECT(''name'' : name, ''age'' : age) AS data FROM users',
  '1m', 'DIFFERENTIAL');

-- JSON_ARRAY: construct a JSON array from values
SELECT pgtrickle.create_stream_table('value_arrays',
  'SELECT id, JSON_ARRAY(a, b, c) AS arr FROM measurements',
  '1m', 'FULL');

-- JSON(): parse a text value as JSON
-- JSON_SCALAR(): wrap a scalar value as JSON
-- JSON_SERIALIZE(): serialize a JSON value to text
```

> **Note:** `JSON_ARRAYAGG()` and `JSON_OBJECTAGG()` are SQL-standard aggregate functions fully recognized by the DVM engine. In DIFFERENTIAL mode, they use the group-rescan strategy (affected groups are re-aggregated from source data). The full deparsed SQL is preserved to handle the special `key: value`, `ABSENT ON NULL`, `ORDER BY`, and `RETURNING` clause syntax.

### JSON_TABLE (PostgreSQL 17+)

`JSON_TABLE()` generates a relational table from JSON data. It is supported in the FROM clause in both FULL and DIFFERENTIAL modes. Internally, it is modeled as a `LateralFunction`.

```sql
-- Extract structured data from a JSON column
SELECT pgtrickle.create_stream_table('user_phones',
  $$SELECT u.id, j.phone_type, j.phone_number
    FROM users u,
         JSON_TABLE(u.contact_info, '$.phones[*]'
           COLUMNS (
             phone_type TEXT PATH '$.type',
             phone_number TEXT PATH '$.number'
           )
         ) AS j$$,
  '1m', 'DIFFERENTIAL');
```

Supported column types:
- **Regular columns** — `name TYPE PATH '$.path'` (with optional `ON ERROR`/`ON EMPTY` behaviors)
- **EXISTS columns** — `name TYPE EXISTS PATH '$.path'`
- **Formatted columns** — `name TYPE FORMAT JSON PATH '$.path'`
- **Nested columns** — `NESTED PATH '$.path' COLUMNS (...)`

The `PASSING` clause is also supported for passing named variables to path expressions.

### Unsupported Expression Types

The following are **rejected with clear error messages** rather than producing broken SQL:

| Expression | Error Behavior | Suggested Rewrite |
|---|---|---|
| `TABLESAMPLE` | Rejected — stream tables materialize the complete result set | Use `WHERE random() < 0.1` if sampling is needed |
| Window functions in expressions | Rejected — e.g., `CASE WHEN ROW_NUMBER() OVER (...) ...` | Move window function to a separate column |
| `FOR UPDATE` / `FOR SHARE` | Rejected — stream tables do not support row-level locking | Remove the locking clause |
| Unknown node types | Rejected with type information | — |

---

## Restrictions & Interoperability

Stream tables are standard PostgreSQL heap tables stored in the `pgtrickle` schema with an additional `__pgt_row_id BIGINT PRIMARY KEY` column managed by the refresh engine. This section describes what you can and cannot do with them.

### Referencing Other Stream Tables

Stream tables **can** reference other stream tables in their defining query. This creates a dependency edge in the internal DAG, and the scheduler refreshes upstream tables before downstream ones. Cycles are detected and rejected at creation time.

```sql
-- ST1 reads from a base table
SELECT pgtrickle.create_stream_table('order_totals',
  'SELECT customer_id, SUM(amount) AS total FROM orders GROUP BY customer_id',
  '1m', 'DIFFERENTIAL');

-- ST2 reads from ST1
SELECT pgtrickle.create_stream_table('big_customers',
  'SELECT customer_id, total FROM pgtrickle.order_totals WHERE total > 1000',
  '1m', 'DIFFERENTIAL');
```

### Views as Sources in Defining Queries

PostgreSQL views **can** be used as source tables in a stream table's defining query. Views are automatically **inlined** — replaced with their underlying SELECT definition as subqueries — so CDC triggers land on the actual base tables.

```sql
CREATE VIEW active_orders AS
  SELECT * FROM orders WHERE status = 'active';

-- This works in DIFFERENTIAL mode:
SELECT pgtrickle.create_stream_table('order_summary',
  'SELECT customer_id, COUNT(*) FROM active_orders GROUP BY customer_id',
  '1m', 'DIFFERENTIAL');
-- Internally, 'active_orders' is replaced with:
--   (SELECT ... FROM orders WHERE status = 'active') AS active_orders
```

**Nested views** (view → view → table) are fully expanded via a fixpoint loop. **Column-renaming views** (`CREATE VIEW v(a, b) AS ...`) work correctly — `pg_get_viewdef()` produces the proper column aliases.

When a view is inlined, the user's original SQL is stored in the `original_query` catalog column for reinit and introspection. The `defining_query` column contains the expanded (post-inlining) form.

**DDL hooks:** `CREATE OR REPLACE VIEW` on a view that was inlined into a stream table marks that ST for reinit. `DROP VIEW` sets affected STs to ERROR status.

**Materialized views** are **rejected** in DIFFERENTIAL mode — their stale-snapshot semantics prevent CDC triggers from tracking changes.  Use the underlying query directly, or switch to FULL mode. In FULL mode, materialized views are allowed (no CDC needed).

**Foreign tables** are **rejected** in DIFFERENTIAL mode — row-level triggers cannot be created on foreign tables. Use FULL mode instead.

### Partitioned Tables as Sources

**Partitioned tables are fully supported** as source tables in both FULL and DIFFERENTIAL modes. CDC triggers are installed on the partitioned parent table, and PostgreSQL 13+ ensures the trigger fires for all DML routed to child partitions. The change buffer uses the parent table's OID (`pgtrickle_changes.changes_<parent_oid>`).

```sql
CREATE TABLE orders (
    id INT, region TEXT, amount NUMERIC
) PARTITION BY LIST (region);
CREATE TABLE orders_us PARTITION OF orders FOR VALUES IN ('US');
CREATE TABLE orders_eu PARTITION OF orders FOR VALUES IN ('EU');

-- Works — inserts into any partition are captured:
SELECT pgtrickle.create_stream_table('order_summary',
  'SELECT region, SUM(amount) FROM orders GROUP BY region',
  '1m', 'DIFFERENTIAL');
```

> **Note:** pg_trickle targets PostgreSQL 18. On PostgreSQL 12 or earlier (not supported), parent triggers do **not** fire for partition-routed rows, which would cause silent data loss.

### Logical Replication Targets

Tables that receive data via **logical replication** require special consideration. Changes arriving via replication do **not** fire normal row-level triggers, which means CDC triggers will miss those changes.

pg_trickle emits a **WARNING** at stream table creation time if any source table is detected as a logical replication target (via `pg_subscription_rel`).

**Workarounds:**
- Use `cdc_mode = 'wal'` for WAL-based CDC that captures all changes regardless of origin.
- Use `FULL` refresh mode, which recomputes entirely from the current table state.
- Set a frequent refresh schedule with FULL mode to limit staleness.

### Views on Stream Tables

PostgreSQL views **can** reference stream tables. The view reflects the data as of the most recent refresh.

```sql
CREATE VIEW top_customers AS
SELECT customer_id, total
FROM pgtrickle.order_totals
WHERE total > 500
ORDER BY total DESC;
```

### Materialized Views on Stream Tables

Materialized views **can** reference stream tables, though this is typically redundant (both are physical snapshots of a query). The materialized view requires its own `REFRESH MATERIALIZED VIEW` — it does **not** auto-refresh when the stream table refreshes.

### Logical Replication of Stream Tables

Stream tables **can** be published for logical replication like any ordinary table:

```sql
-- On publisher
CREATE PUBLICATION my_pub FOR TABLE pgtrickle.order_totals;

-- On subscriber
CREATE SUBSCRIPTION my_sub
  CONNECTION 'host=... dbname=...'
  PUBLICATION my_pub;
```

**Caveats:**
- The `__pgt_row_id` column is replicated (it is the primary key), which is an internal implementation detail.
- The subscriber receives materialized data, not the defining query. Refreshes on the publisher propagate as normal DML via logical replication.
- Do **not** install pg_trickle on the subscriber and attempt to refresh the replicated table — it will have no CDC triggers or catalog entries.
- The internal change buffer tables (`pgtrickle_changes.changes_<oid>`) and catalog tables are **not** published by default; subscribers only receive the final output.

### Known Delta Computation Limitations

The following edge cases produce incorrect delta results in DIFFERENTIAL mode under specific
data mutation patterns. They have no effect on FULL mode.

#### JOIN Key Column Change + Simultaneous Right-Side Delete

When a row's join key column is updated **in the same refresh cycle** as the joined-side row is deleted,
the delta query may fail to emit the required DELETE from the stream table:

```sql
-- Stream table joining orders with customers
SELECT pgtrickle.create_stream_table('order_details',
  'SELECT o.id, c.name FROM orders o JOIN customers c ON o.cust_id = c.id',
  '1m', 'DIFFERENTIAL');

-- Scenario that exposes the limitation:
-- In the same transaction (or same refresh interval):
UPDATE orders SET cust_id = 5 WHERE cust_id = 3;  -- key change
DELETE FROM customers WHERE id = 3;               -- old join partner deleted
-- The delta for the now-stale (orders.cust_id=3, customers.id=3) join result
-- may not be emitted as a DELETE, leaving a stale row in the stream table
-- until the next full refresh cycle.
```

**Root cause:** The JOIN delta query reads `current_right` (customers) after all changes are applied.
When customer 3 is deleted before the delta runs, the DELETE half of the join cannot find its join
partner and is silently dropped.

**Mitigations:**
- **Adaptive FULL fallback** (default): when the scheduler detects a high change volume, it switches
  to a full recompute, which will correct any stale rows. The threshold is configurable via
  `pg_trickle.adaptive_full_threshold`.
- **Avoid co-locating key-changing UPDATEs and DELETEs** in the same refresh interval. Stagger
  changes across multiple refresh cycles.
- **FULL mode** for stream tables where join key changes and right-side deletes are expected to
  co-occur frequently.

#### CUBE/ROLLUP Expansion Limit

`CUBE(a, b, c...n)` on **N** columns generates $2^N$ grouping set branches (a UNION ALL of N queries).
pg_trickle rejects CUBE/ROLLUP that would produce more than **64 branches** to prevent runaway
memory usage during query generation. Use explicit `GROUPING SETS(...)` instead:

```sql
-- Rejected: CUBE(a, b, c, d, e, f, g) would generate 128 branches
-- Use instead:
SELECT pgtrickle.create_stream_table('multi_dim',
  'SELECT a, b, c, SUM(v) FROM t
   GROUP BY GROUPING SETS ((a, b, c), (a, b), (a), ())',
  '5m', 'DIFFERENTIAL');
```

### What Is NOT Allowed

| Operation | Restriction | Reason |
|---|---|---|
| Direct DML (`INSERT`, `UPDATE`, `DELETE`) | ❌ Not supported | Stream table contents are managed exclusively by the refresh engine. |
| Direct DDL (`ALTER TABLE`) | ❌ Not supported | Use `pgtrickle.alter_stream_table()` to change the defining query or schedule. |
| Foreign keys referencing or from a stream table | ❌ Not supported | The refresh engine performs bulk `MERGE` operations that do not respect FK ordering. |
| User-defined triggers on stream tables | ✅ Supported (DIFFERENTIAL) | In DIFFERENTIAL mode, the refresh engine decomposes changes into explicit DELETE + UPDATE + INSERT statements so triggers fire with correct `TG_OP`, `OLD`, and `NEW`. Row-level triggers are suppressed during FULL refresh. Controlled by `pg_trickle.user_triggers` GUC (default: `auto`). |
| `TRUNCATE` on a stream table | ❌ Not supported | Use `pgtrickle.refresh_stream_table()` to reset data. |

> **Tip:** The `__pgt_row_id` column is visible but should be ignored by consuming queries — it is an implementation detail used for delta `MERGE` operations.

---

## Views

### pgtrickle.stream_tables_info

Status overview with computed staleness information.

```sql
SELECT * FROM pgtrickle.stream_tables_info;
```

Columns include all `pgtrickle.pgt_stream_tables` columns plus:

| Column | Type | Description |
|---|---|---|
| `staleness` | `interval` | `now() - data_timestamp` |
| `stale` | `bool` | `true` if `staleness > schedule` |

---

### pgtrickle.pg_stat_stream_tables

Comprehensive monitoring view combining catalog metadata with aggregate refresh statistics.

```sql
SELECT * FROM pgtrickle.pg_stat_stream_tables;
```

Key columns:

| Column | Type | Description |
|---|---|---|
| `pgt_id` | `bigint` | Stream table ID |
| `pgt_schema` / `pgt_name` | `text` | Schema and name |
| `status` | `text` | INITIALIZING, ACTIVE, SUSPENDED, ERROR |
| `refresh_mode` | `text` | FULL or DIFFERENTIAL |
| `data_timestamp` | `timestamptz` | Timestamp of last refresh |
| `staleness` | `interval` | Current staleness |
| `stale` | `bool` | Whether schedule is exceeded |
| `total_refreshes` | `bigint` | Total refresh count |
| `successful_refreshes` | `bigint` | Successful refresh count |
| `failed_refreshes` | `bigint` | Failed refresh count |
| `avg_duration_ms` | `float8` | Average refresh duration |
| `consecutive_errors` | `int` | Current error streak |

---

## Catalog Tables

### pgtrickle.pgt_stream_tables

Core metadata for each stream table.

| Column | Type | Description |
|---|---|---|
| `pgt_id` | `bigserial` | Primary key |
| `pgt_relid` | `oid` | OID of the storage table |
| `pgt_name` | `text` | Table name |
| `pgt_schema` | `text` | Schema name |
| `defining_query` | `text` | The SQL query that defines the ST |
| `schedule` | `text` | Refresh schedule (duration or cron expression) |
| `refresh_mode` | `text` | FULL or DIFFERENTIAL |
| `status` | `text` | INITIALIZING, ACTIVE, SUSPENDED, ERROR |
| `is_populated` | `bool` | Whether the table has been populated |
| `data_timestamp` | `timestamptz` | Timestamp of the data in the ST |
| `frontier` | `jsonb` | Per-source LSN positions (version tracking) |
| `last_refresh_at` | `timestamptz` | When last refreshed |
| `consecutive_errors` | `int` | Current error streak count |
| `needs_reinit` | `bool` | Whether upstream DDL requires reinitialization |
| `functions_used` | `text[]` | Function names used in the defining query (for DDL tracking) |
| `created_at` | `timestamptz` | Creation timestamp |
| `updated_at` | `timestamptz` | Last modification timestamp |

### pgtrickle.pgt_dependencies

DAG edges — records which source tables each ST depends on, including CDC mode metadata.

| Column | Type | Description |
|---|---|---|
| `pgt_id` | `bigint` | FK to pgt_stream_tables |
| `source_relid` | `oid` | OID of the source table |
| `source_type` | `text` | TABLE, STREAM_TABLE, or VIEW |
| `columns_used` | `text[]` | Which columns are referenced |
| `cdc_mode` | `text` | Current CDC mode: TRIGGER, TRANSITIONING, or WAL |
| `slot_name` | `text` | Replication slot name (WAL/TRANSITIONING modes) |
| `decoder_confirmed_lsn` | `pg_lsn` | WAL decoder's last confirmed position |
| `transition_started_at` | `timestamptz` | When the trigger→WAL transition started |

### pgtrickle.pgt_refresh_history

Audit log of all refresh operations.

| Column | Type | Description |
|---|---|---|
| `refresh_id` | `bigserial` | Primary key |
| `pgt_id` | `bigint` | FK to pgt_stream_tables |
| `data_timestamp` | `timestamptz` | Data timestamp of the refresh |
| `start_time` | `timestamptz` | When the refresh started |
| `end_time` | `timestamptz` | When it completed |
| `action` | `text` | NO_DATA, FULL, DIFFERENTIAL, REINITIALIZE, SKIP |
| `rows_inserted` | `bigint` | Rows inserted |
| `rows_deleted` | `bigint` | Rows deleted |
| `error_message` | `text` | Error message if failed |
| `status` | `text` | RUNNING, COMPLETED, FAILED, SKIPPED |
| `initiated_by` | `text` | What triggered: SCHEDULER, MANUAL, or INITIAL |
| `freshness_deadline` | `timestamptz` | SLA deadline (duration schedules only; NULL for cron) |

### pgtrickle.pgt_change_tracking

CDC slot tracking per source table.

| Column | Type | Description |
|---|---|---|
| `source_relid` | `oid` | OID of the tracked source table |
| `slot_name` | `text` | Logical replication slot name |
| `last_consumed_lsn` | `pg_lsn` | Last consumed WAL position |
| `tracked_by_pgt_ids` | `bigint[]` | Array of ST IDs depending on this source |
