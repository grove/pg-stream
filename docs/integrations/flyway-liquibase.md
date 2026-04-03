# Flyway & Liquibase Migration Frameworks

pg_trickle stream tables are managed through SQL function calls, not standard
DDL (`CREATE TABLE` / `ALTER TABLE`). This page documents patterns for
integrating pg_trickle with Flyway and Liquibase migration frameworks.

## Key Principle

Stream tables are created and managed via `pgtrickle.create_stream_table()`,
`pgtrickle.alter_stream_table()`, and `pgtrickle.drop_stream_table()`. These
are regular SQL function calls that can be embedded in any migration script.

CDC triggers are automatically installed on source tables during stream table
creation — no manual trigger management is needed.

---

## Flyway

### Creating Stream Tables in Migrations

Place stream table definitions in versioned migration files alongside your
regular schema changes:

```sql
-- V3__create_order_stream_tables.sql

-- 1. Create the source tables first (standard DDL)
CREATE TABLE IF NOT EXISTS orders (
    id         SERIAL PRIMARY KEY,
    region     TEXT NOT NULL,
    amount     NUMERIC(10,2) NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now()
);

-- 2. Create stream tables via pg_trickle API
SELECT pgtrickle.create_stream_table(
    'order_totals',
    $$SELECT region, COUNT(*) AS order_count, SUM(amount) AS total
      FROM orders GROUP BY region$$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

### Altering Stream Tables

Use `pgtrickle.alter_stream_table()` in a new migration:

```sql
-- V5__update_order_totals_schedule.sql
SELECT pgtrickle.alter_stream_table(
    'order_totals',
    schedule => '10s'
);
```

### Altering the Defining Query

Use `alter_query` to change the SQL without dropping and recreating:

```sql
-- V7__add_avg_to_order_totals.sql
SELECT pgtrickle.alter_stream_table(
    'order_totals',
    alter_query => $$SELECT region,
                            COUNT(*) AS order_count,
                            SUM(amount) AS total,
                            AVG(amount) AS avg_amount
                     FROM orders GROUP BY region$$
);
```

### Dropping Stream Tables

```sql
-- V9__remove_legacy_stream_tables.sql
SELECT pgtrickle.drop_stream_table('legacy_report');
```

### Bulk Creation

For environments with many stream tables, use `bulk_create` to create
them atomically:

```sql
-- V4__create_all_stream_tables.sql
SELECT pgtrickle.bulk_create('[
    {
        "name": "order_totals",
        "query": "SELECT region, COUNT(*) AS cnt, SUM(amount) AS total FROM orders GROUP BY region",
        "schedule": "5s",
        "refresh_mode": "DIFFERENTIAL"
    },
    {
        "name": "daily_revenue",
        "query": "SELECT date_trunc(''day'', created_at) AS day, SUM(amount) AS revenue FROM orders GROUP BY 1",
        "schedule": "30s",
        "refresh_mode": "DIFFERENTIAL"
    }
]'::jsonb);
```

### Ordering: Source Tables Before Stream Tables

Flyway executes migrations in version order. Ensure source tables are created
in an earlier migration than their dependent stream tables:

```
V1__create_schema.sql           -- CREATE TABLE orders, products, ...
V2__create_indexes.sql          -- CREATE INDEX ...
V3__create_stream_tables.sql    -- SELECT pgtrickle.create_stream_table(...)
```

### Repeatable Migrations

If you want stream table definitions to be re-applied on every Flyway run
(for development environments), use repeatable migrations:

```sql
-- R__stream_tables.sql
-- Drop and recreate all stream tables
SELECT pgtrickle.drop_stream_table('order_totals') 
WHERE EXISTS (
    SELECT 1 FROM pgtrickle.pgt_stream_tables 
    WHERE pgt_name = 'order_totals'
);

SELECT pgtrickle.create_stream_table(
    'order_totals',
    $$SELECT region, COUNT(*) AS cnt FROM orders GROUP BY region$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

Or use `create_or_replace_stream_table` for idempotent definitions:

```sql
-- R__stream_tables.sql (idempotent)
SELECT pgtrickle.create_or_replace_stream_table(
    'order_totals',
    $$SELECT region, COUNT(*) AS cnt FROM orders GROUP BY region$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

### Handling `ALTER TABLE` on Source Tables

When a Flyway migration alters a source table (e.g., adding a column),
pg_trickle's DDL event trigger detects the change and suspends affected
stream tables. After the schema change, stream tables resume automatically
on the next refresh cycle.

If the source table change invalidates the stream table's defining query
(e.g., removing a referenced column), you must update or drop the stream
table in the same or a subsequent migration.

---

## Liquibase

### Creating Stream Tables in Changesets

Use Liquibase's `<sql>` tag to call pg_trickle functions:

```xml
<!-- changelog-3.0.xml -->
<changeSet id="create-order-stream-tables" author="dev">
    <sql>
        SELECT pgtrickle.create_stream_table(
            'order_totals',
            $pgt$SELECT region, COUNT(*) AS order_count, SUM(amount) AS total
                  FROM orders GROUP BY region$pgt$,
            schedule     => '5s',
            refresh_mode => 'DIFFERENTIAL'
        );
    </sql>
    <rollback>
        <sql>SELECT pgtrickle.drop_stream_table('order_totals');</sql>
    </rollback>
</changeSet>
```

### Rollback Support

Always include `<rollback>` blocks that drop the stream table:

```xml
<changeSet id="add-daily-revenue-st" author="dev">
    <sql>
        SELECT pgtrickle.create_stream_table(
            'daily_revenue',
            $pgt$SELECT date_trunc('day', created_at) AS day,
                        SUM(amount) AS revenue
                 FROM orders GROUP BY 1$pgt$,
            schedule => '30s',
            refresh_mode => 'DIFFERENTIAL'
        );
    </sql>
    <rollback>
        <sql>SELECT pgtrickle.drop_stream_table('daily_revenue');</sql>
    </rollback>
</changeSet>
```

### Altering Stream Tables

```xml
<changeSet id="update-order-totals-schedule" author="dev">
    <sql>
        SELECT pgtrickle.alter_stream_table(
            'order_totals',
            schedule => '10s'
        );
    </sql>
    <rollback>
        <sql>
            SELECT pgtrickle.alter_stream_table(
                'order_totals',
                schedule => '5s'
            );
        </sql>
    </rollback>
</changeSet>
```

### Preconditions

Use Liquibase preconditions to check whether pg_trickle is available:

```xml
<changeSet id="create-stream-tables" author="dev">
    <preConditions onFail="MARK_RAN">
        <sqlCheck expectedResult="1">
            SELECT COUNT(*) FROM pg_extension WHERE extname = 'pg_trickle'
        </sqlCheck>
    </preConditions>
    <sql>
        SELECT pgtrickle.create_stream_table(...);
    </sql>
</changeSet>
```

---

## Common Patterns

### Environment-Specific Schedules

Use different schedules for development vs. production:

```sql
-- Use a function to parameterize schedules
SELECT pgtrickle.create_stream_table(
    'order_totals',
    $$SELECT region, COUNT(*) AS cnt FROM orders GROUP BY region$$,
    schedule => CASE 
        WHEN current_setting('pg_trickle.enabled', true) = 'on' 
        THEN '5s' 
        ELSE '1m' 
    END,
    refresh_mode => 'DIFFERENTIAL'
);
```

### CI/Test Environments

In CI, set `pg_trickle.enabled = off` in `postgresql.conf` to prevent the
background scheduler from running during schema migrations. Stream tables
will still be created correctly — they just won't auto-refresh until the
scheduler is enabled.

### Extension Dependency

Ensure `CREATE EXTENSION pg_trickle` runs before any stream table migration.
In Flyway, use an early versioned migration:

```sql
-- V0__extensions.sql
CREATE EXTENSION IF NOT EXISTS pg_trickle;
```

In Liquibase:

```xml
<changeSet id="install-extensions" author="dev" runOnChange="true">
    <sql>CREATE EXTENSION IF NOT EXISTS pg_trickle;</sql>
</changeSet>
```

---

## Further Reading

- [SQL Reference](../SQL_REFERENCE.md) — Complete function reference
- [Configuration](../CONFIGURATION.md) — GUC variables for schedule tuning
- [Getting Started](../GETTING_STARTED.md) — First stream table walkthrough
