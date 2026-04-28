[← Back to Blog Index](README.md)

# Parameterized Stream Tables: Building a SQL View Library

## Patterns for reusable, tenant-scoped, and versionable stream table definitions in multi-tenant schemas

---

As your pg_trickle deployment grows, you'll notice a pattern: many stream tables have the same structure, differing only in a filter value. "Revenue by product for tenant A" and "revenue by product for tenant B" are the same query with different WHERE clauses. "Daily orders for the US region" and "daily orders for the EU region" are the same aggregation scoped to different subsets.

The naive approach — creating a separate stream table per tenant or per segment — works but doesn't scale. With 500 tenants, you have 500 nearly identical stream tables, 500 sets of CDC triggers, and a DAG with 500 nodes that are logically the same computation. Changes to the aggregation logic require modifying 500 stream table definitions.

This post explores patterns for building reusable, parameterized stream table definitions that serve multiple tenants or segments efficiently, while maintaining the per-tenant isolation that applications expect.

---

## Pattern 1: Single Stream Table with Tenant Column

The simplest and most efficient pattern is a single stream table that includes the tenant or segment as a grouping column:

```sql
SELECT pgtrickle.create_stream_table(
    'revenue_by_tenant_product',
    $$
    SELECT
        tenant_id,
        product_category,
        date_trunc('day', ordered_at) AS day,
        SUM(amount) AS revenue,
        COUNT(*) AS order_count
    FROM orders
    GROUP BY tenant_id, product_category, date_trunc('day', ordered_at)
    $$
);
```

All tenants share a single stream table. When tenant A places an order, only their group is updated. Tenant B's rows are untouched. The incremental engine is already group-aware — it processes deltas per group, so adding `tenant_id` as a grouping column doesn't change the fundamental cost model.

Applications filter at query time:

```sql
-- Application query (per-tenant dashboard)
SELECT product_category, day, revenue, order_count
FROM revenue_by_tenant_product
WHERE tenant_id = 'tenant_abc'
ORDER BY day DESC;
```

With an index on `(tenant_id, day)`, this is a fast point lookup. No sequential scan, no cross-tenant data leakage.

**Advantages:**
- Single stream table to manage (one definition, one DAG node)
- Shared CDC infrastructure (one trigger per source table)
- Efficient index-based tenant isolation at query time
- Adding a new tenant requires no schema changes

**Disadvantages:**
- All tenants must use the same aggregation logic
- Row-level security (RLS) adds a performance tax on reads
- Very large tenants and very small tenants share the same refresh cadence

---

## Pattern 2: Template Functions for Tenant-Specific Tables

When tenants need different schemas or different refresh cadences, you can use a template function that generates tenant-specific stream tables:

```sql
-- Template function: creates a per-tenant stream table
CREATE OR REPLACE FUNCTION create_tenant_analytics(p_tenant_id text)
RETURNS void AS $$
BEGIN
    PERFORM pgtrickle.create_stream_table(
        'analytics_' || p_tenant_id,
        format(
            'SELECT
                product_category,
                date_trunc(''day'', ordered_at) AS day,
                SUM(amount) AS revenue,
                COUNT(*) AS order_count
            FROM orders
            WHERE tenant_id = %L
            GROUP BY product_category, date_trunc(''day'', ordered_at)',
            p_tenant_id
        )
    );
END;
$$ LANGUAGE plpgsql;

-- Create analytics for a new tenant
SELECT create_tenant_analytics('tenant_abc');
SELECT create_tenant_analytics('tenant_xyz');
```

Each tenant gets their own stream table with a WHERE filter. pg_trickle's incremental engine evaluates each stream table's filter against incoming changes, so an order for tenant_abc only triggers a refresh on `analytics_tenant_abc` — not on `analytics_tenant_xyz`.

**Advantages:**
- Per-tenant refresh cadence and configuration
- Per-tenant table permissions (no RLS needed)
- Tenants can have different schemas if needed

**Disadvantages:**
- More DAG nodes (one per tenant)
- More CDC overhead (each source table change is evaluated against all tenant filters)
- Schema management complexity (updates require iterating over all tenants)

---

## Pattern 3: Schema-Per-Tenant Isolation

For strict multi-tenant isolation (common in B2B SaaS with compliance requirements), each tenant has their own schema:

```sql
-- Tenant provisioning: create schema and stream tables
CREATE OR REPLACE FUNCTION provision_tenant(p_tenant text)
RETURNS void AS $$
BEGIN
    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I', p_tenant);

    EXECUTE format(
        'CREATE TABLE %I.orders (
            id serial PRIMARY KEY,
            product_category text,
            amount numeric,
            ordered_at timestamptz DEFAULT now()
        )', p_tenant
    );

    -- Stream table in tenant schema
    PERFORM pgtrickle.create_stream_table(
        p_tenant || '.daily_revenue',
        format(
            'SELECT
                product_category,
                date_trunc(''day'', ordered_at) AS day,
                SUM(amount) AS revenue,
                COUNT(*) AS order_count
            FROM %I.orders
            GROUP BY product_category, date_trunc(''day'', ordered_at)',
            p_tenant
        )
    );
END;
$$ LANGUAGE plpgsql;
```

Each tenant is completely isolated — different schemas, different source tables, different stream tables. pg_trickle manages each independently. This is the most isolated but also the most resource-intensive pattern.

---

## Pattern 4: Versioned Definitions

As your analytics evolve, you need to update stream table definitions without breaking existing consumers. A versioning pattern:

```sql
-- Version 1: basic revenue aggregation
SELECT pgtrickle.create_stream_table(
    'revenue_v1',
    $$
    SELECT
        tenant_id,
        date_trunc('day', ordered_at) AS day,
        SUM(amount) AS revenue
    FROM orders
    GROUP BY tenant_id, date_trunc('day', ordered_at)
    $$
);

-- Version 2: adds product category and order count
SELECT pgtrickle.create_stream_table(
    'revenue_v2',
    $$
    SELECT
        tenant_id,
        product_category,
        date_trunc('day', ordered_at) AS day,
        SUM(amount) AS revenue,
        COUNT(*) AS order_count,
        AVG(amount) AS avg_order_value
    FROM orders
    GROUP BY tenant_id, product_category, date_trunc('day', ordered_at)
    $$
);
```

Both versions coexist. Applications on the old API continue reading from `revenue_v1`. New features use `revenue_v2`. Both are maintained incrementally from the same source table. Once all consumers have migrated, drop v1.

For more sophisticated versioning, use views as the public interface:

```sql
-- Public interface: a view that points to the current version
CREATE VIEW revenue_current AS SELECT * FROM revenue_v2;

-- When v3 is ready:
-- 1. Create revenue_v3 stream table
-- 2. ALTER VIEW revenue_current AS SELECT * FROM revenue_v3;
-- 3. Drop revenue_v2 after migration period
```

---

## Pattern 5: Composable Building Blocks

Build a library of reusable intermediate stream tables that serve as building blocks for application-specific views:

```sql
-- Building block 1: Order facts (shared by all analytics)
SELECT pgtrickle.create_stream_table(
    'order_facts',
    $$
    SELECT
        o.id AS order_id,
        o.tenant_id,
        o.customer_id,
        c.segment AS customer_segment,
        o.product_category,
        o.amount,
        o.ordered_at,
        date_trunc('day', o.ordered_at) AS order_day,
        date_trunc('week', o.ordered_at) AS order_week,
        date_trunc('month', o.ordered_at) AS order_month
    FROM orders o
    JOIN customers c ON c.id = o.customer_id
    $$
);

-- Building block 2: Daily aggregates (built on order_facts)
SELECT pgtrickle.create_stream_table(
    'daily_metrics',
    $$
    SELECT
        tenant_id,
        product_category,
        customer_segment,
        order_day,
        SUM(amount) AS revenue,
        COUNT(*) AS orders,
        COUNT(DISTINCT customer_id) AS unique_customers
    FROM order_facts
    GROUP BY tenant_id, product_category, customer_segment, order_day
    $$
);

-- Application view: monthly trends (built on daily_metrics)
SELECT pgtrickle.create_stream_table(
    'monthly_trends',
    $$
    SELECT
        tenant_id,
        product_category,
        date_trunc('month', order_day) AS month,
        SUM(revenue) AS monthly_revenue,
        SUM(orders) AS monthly_orders,
        SUM(unique_customers) AS monthly_customers
    FROM daily_metrics
    GROUP BY tenant_id, product_category, date_trunc('month', order_day)
    $$
);
```

The DAG is: orders → order_facts → daily_metrics → monthly_trends. Each layer adds aggregation. New application views can be built on any layer without touching the layers below. Need "weekly revenue by segment"? Build it on `daily_metrics`. Need "top customers by lifetime value"? Build it on `order_facts`.

---

## Pattern 6: Configuration-Driven Definitions

For platforms that offer self-service analytics (users define their own dashboards), store stream table definitions as configuration:

```sql
-- Stream table definition registry
CREATE TABLE stream_table_registry (
    id           serial PRIMARY KEY,
    name         text NOT NULL UNIQUE,
    tenant_id    text,
    definition   text NOT NULL,  -- SQL query
    refresh_mode text DEFAULT 'DEFERRED',
    version      integer DEFAULT 1,
    created_at   timestamptz DEFAULT now(),
    is_active    boolean DEFAULT true
);

-- Deploy a definition
CREATE OR REPLACE FUNCTION deploy_stream_definition(p_name text)
RETURNS void AS $$
DECLARE
    v_def record;
BEGIN
    SELECT * INTO v_def FROM stream_table_registry WHERE name = p_name AND is_active;
    IF NOT FOUND THEN RAISE EXCEPTION 'Definition not found: %', p_name; END IF;

    -- Drop existing if version changed
    PERFORM pgtrickle.drop_stream_table(p_name);
    PERFORM pgtrickle.create_stream_table(p_name, v_def.definition);
    PERFORM pgtrickle.alter_stream_table(p_name, refresh_mode := v_def.refresh_mode);
END;
$$ LANGUAGE plpgsql;
```

This pattern enables infrastructure-as-code for stream tables. Definitions are stored in a registry, versioned, and deployed via function calls. CI/CD pipelines can manage stream table deployments the same way they manage schema migrations.

---

## Row-Level Security for Shared Tables

When multiple tenants share a single stream table, PostgreSQL's RLS provides the isolation:

```sql
-- Enable RLS on the shared stream table
ALTER TABLE revenue_by_tenant_product ENABLE ROW LEVEL SECURITY;

-- Policy: each role can only see their tenant's data
CREATE POLICY tenant_isolation ON revenue_by_tenant_product
    FOR SELECT
    USING (tenant_id = current_setting('app.current_tenant'));
```

Applications set `app.current_tenant` at connection time, and RLS transparently filters results. The stream table itself contains all tenants' data (efficient for incremental maintenance), but each tenant only sees their own rows.

---

## Choosing the Right Pattern

| Scenario | Recommended pattern |
|----------|-------------------|
| 10–50 tenants, same analytics | Pattern 1 (single table + tenant column) |
| 50–500 tenants, same analytics | Pattern 1 + RLS |
| Tenants with different schemas | Pattern 2 (template functions) |
| Strict compliance isolation | Pattern 3 (schema-per-tenant) |
| Evolving analytics definitions | Pattern 4 (versioned) |
| Complex analytics stack | Pattern 5 (composable blocks) |
| Self-service analytics platform | Pattern 6 (config-driven) |

Most applications should start with Pattern 1. It's the simplest, most efficient, and handles the majority of multi-tenant use cases. Move to more complex patterns only when the requirements demand it.

---

*Stream tables don't have to be one-off definitions. Build a library of composable, versioned, tenant-aware analytics that grow with your product — without multiplicative infrastructure costs.*
