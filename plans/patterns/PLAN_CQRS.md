# CQRS Pattern with pg_trickle

> **Status:** Research Report
> **Created:** 2026-04-17
> **Category:** Architecture Pattern
> **Related:** [PLAN_TRANSACTIONAL_OUTBOX.md](PLAN_TRANSACTIONAL_OUTBOX.md) · [PLAN_TRANSACTIONAL_INBOX.md](PLAN_TRANSACTIONAL_INBOX.md)

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [What Is CQRS?](#what-is-cqrs)
- [The Problem It Solves](#the-problem-it-solves)
- [How pg_trickle Enables CQRS](#how-pg_trickle-enables-cqrs)
  - [Architecture Overview](#architecture-overview)
  - [Approach 1: Single-Service CQRS (Multiple Read Models)](#approach-1-single-service-cqrs-multiple-read-models)
  - [Approach 2: Multi-Service CQRS (Microservices)](#approach-2-multi-service-cqrs-microservices)
  - [Approach 3: Event Sourcing + CQRS (Combined Pattern)](#approach-3-event-sourcing--cqrs-combined-pattern)
- [Read Model Design Strategies](#read-model-design-strategies)
  - [Strategy A: Operational Read Models (Sub-Second Freshness)](#strategy-a-operational-read-models-sub-second-freshness)
  - [Strategy B: Analytics Read Models (Aggregated)](#strategy-b-analytics-read-models-aggregated)
  - [Strategy C: Cross-Service Read Models (Denormalized Joins)](#strategy-c-cross-service-read-models-denormalized-joins)
- [Worked Example: E-Commerce Order Service](#worked-example-e-commerce-order-service)
- [Read Model Versioning](#read-model-versioning)
- [Consistency Trade-Offs](#consistency-trade-offs)
- [Complementary PostgreSQL Extensions](#complementary-postgresql-extensions)
  - [pgvector — Semantic Read Models](#pgvector--semantic-read-models)
  - [pg_cron — Time-Triggered Projections](#pg_cron--time-triggered-projections)
  - [pgmq — Command Queue](#pgmq--command-queue)
- [Potential pg_trickle Extensions](#potential-pg_trickle-extensions)
  - [Extension 1: Read Model Registry](#extension-1-read-model-registry)
  - [Extension 2: Staleness Monitoring per Read Model](#extension-2-staleness-monitoring-per-read-model)
- [Observability & Monitoring](#observability--monitoring)
- [Testing Strategies](#testing-strategies)
- [Security Considerations](#security-considerations)
- [Cost Analysis](#cost-analysis)
- [Comparison with Traditional Approaches](#comparison-with-traditional-approaches)
- [When NOT to Use CQRS](#when-not-to-use-cqrs)
- [References](#references)

---

## Executive Summary

**Command Query Responsibility Segregation (CQRS)** is an architectural
pattern that separates the operations that *change state* (commands) from the
operations that *read state* (queries). The key insight is that write-optimized
and read-optimized data models are fundamentally different, and forcing one
schema to serve both creates unnecessary compromise.

pg_trickle makes CQRS practical in PostgreSQL by:

1. **Stream tables are purpose-built read models.** Each stream table is an
   always-fresh, pre-computed projection of the command-side source table,
   optimized exactly for the query it serves.
2. **CDC triggers capture every command-side change automatically.** No
   manual "projection rebuilder" process is needed — pg_trickle handles
   incremental view maintenance continuously.
3. **DIFFERENTIAL refresh** means read models update in microseconds for
   typical workloads — not seconds.
4. **Multiple independent read models** can share a single CDC change buffer.
   The cost of CDC is paid once regardless of how many projections read from
   the same source.
5. **IMMEDIATE mode** bridges CQRS into transactionally-consistent read
   models when eventual consistency is not acceptable.

---

## What Is CQRS?

The term was coined by Greg Young and Udi Dahan as an evolution of the
Command-Query Separation (CQS) principle by Bertrand Meyer. CQRS scales it up
from individual methods to the whole service layer.

**The core idea:**

```
  ┌─────────────────────────────────────────────┐
  │                 Application                  │
  │                                              │
  │   Commands              Queries              │
  │   (create, update,      (read, search,       │
  │    delete, process)      filter, aggregate)  │
  │        │                      │              │
  │        ▼                      ▼              │
  │  ┌──────────┐          ┌──────────────┐      │
  │  │  Write   │          │  Read Model  │      │
  │  │  Model   │  syncs   │  (stream     │      │
  │  │  (source │ ───────→ │   tables)    │      │
  │  │   table) │          │              │      │
  │  └──────────┘          └──────────────┘      │
  └─────────────────────────────────────────────┘
```

**Without CQRS:** A single normalized schema optimized for writes is also
asked to handle complex analytical queries. Developers add indexes for reads
that slow down writes, or denormalize for reads in ways that complicate writes.
The result is neither fast at writes nor fast at reads.

**With CQRS + pg_trickle:** The write model is kept lean and normalized,
optimized purely for transactional correctness. Each read model is a separate
stream table — a pre-computed, perfectly shaped view of the data, updated
automatically and incrementally.

---

## The Problem It Solves

### Symptom 1: The "One Schema for Everything" Bottleneck

```sql
-- Command side needs: fast single-row lookup by primary key
SELECT * FROM orders WHERE id = 42;  -- needs: PK index only

-- Query side needs: multi-dimensional aggregation
SELECT region, product_category, date_trunc('week', created_at),
       SUM(amount), AVG(delivery_days), percentile_disc(0.95)...
FROM orders
JOIN customers ON ...
JOIN products ON ...
WHERE status = 'delivered' AND created_at > ...
GROUP BY 1, 2, 3;
-- needs: composite indexes on (status, created_at, region, ...)
-- those same indexes slow down every INSERT/UPDATE
```

Adding read-optimized indexes degrades write throughput. Denormalizing for
reads violates write-time referential integrity constraints. The schema becomes
a compromise that is suboptimal for both use cases.

### Symptom 2: Lock Contention Under Mixed Workloads

Long-running analytical queries hold shared locks that block or are blocked by
concurrent writes. CQRS eliminates this: the read model (stream table) is a
**separate physical table** — analytical queries read from it without touching
the command-side source table at all.

### Symptom 3: Every Query is an N-table Join

```sql
-- Without CQRS, a "simple" dashboard query requires:
SELECT c.name, c.tier, SUM(o.amount), COUNT(o.id), MAX(o.created_at)
FROM customers c
JOIN orders o ON o.customer_id = c.id
JOIN products p ON p.id = o.product_id
LEFT JOIN promotions pr ON pr.order_id = o.id
WHERE o.created_at > now() - '30 days'::interval
GROUP BY c.id, c.name, c.tier;
-- Executed thousands of times per minute by the dashboard.
```

With CQRS, this query is pre-computed once (or incrementally via DIFFERENTIAL),
and the dashboard reads from a single pre-joined table.

| Problem                     | Without CQRS              | With CQRS + pg_trickle     |
|-----------------------------|---------------------------|----------------------------|
| Analytical query latency    | 50ms–10s                  | < 1ms (pre-computed)       |
| Write throughput            | Degraded by read indexes  | Unaffected                 |
| Lock contention             | High (shared reads block) | None (separate table)      |
| Schema complexity           | Compromise schema         | Optimal per use case       |
| Query complexity            | Repeated N-way joins      | Simple SELECT on flat table|

---

## How pg_trickle Enables CQRS

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                            PostgreSQL                                │
│                                                                      │
│  ┌──────────────────────────────────────┐                           │
│  │  COMMAND SIDE (Write Model)           │                           │
│  │                                       │                           │
│  │  orders          customers            │                           │
│  │  (normalized,    (normalized,         │                           │
│  │   write-only     write-only           │                           │
│  │   indexes)       indexes)             │                           │
│  └────────────┬─────────────────────────┘                           │
│               │ CDC triggers (automatic, within same transaction)    │
│               ▼                                                      │
│  ┌──────────────────────────────────────┐                           │
│  │  pgtrickle_changes (change buffers)   │                           │
│  └────────────┬─────────────────────────┘                           │
│               │ DIFFERENTIAL refresh (incremental, low-cost)        │
│               ▼                                                      │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  READ MODELS (Stream Tables — query side)                     │   │
│  │                                                               │   │
│  │  customer_360         order_dashboard      product_catalog    │   │
│  │  (denormalized        (pre-aggregated,     (enriched,         │   │
│  │   customer view,       real-time metrics,   search-ready,     │   │
│  │   30s schedule)        5s schedule)         1m schedule)      │   │
│  │                                                               │   │
│  │  fraud_signals        settlement_report    inventory_status   │   │
│  │  (IMMEDIATE mode       (daily rollup,       (calculated,      │   │
│  │   per-transaction)     cold tier)           chain from orders)│   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                      │
│  Applications read ONLY from stream tables (never from base tables) │
└─────────────────────────────────────────────────────────────────────┘
```

**Key principle:** Application queries go to stream tables. Only commands
(INSERT/UPDATE/DELETE) touch source tables. The stream tables are the public
read API.

### Approach 1: Single-Service CQRS (Multiple Read Models)

The simplest CQRS deployment: one PostgreSQL database, one source table, many
read models.

```sql
-- COMMAND SIDE: Lean, write-optimized table
-- Minimal indexes (only what's needed for FK integrity and PK lookups)
CREATE TABLE orders (
    id          BIGSERIAL PRIMARY KEY,
    customer_id INT NOT NULL REFERENCES customers(id),
    status      TEXT NOT NULL DEFAULT 'pending',
    amount      NUMERIC(12, 2) NOT NULL,
    region      TEXT NOT NULL,
    product_id  INT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Only three indexes: PK, FK, and a partial index for the scheduler
CREATE INDEX ON orders (customer_id);
CREATE INDEX ON orders (updated_at) WHERE status != 'archived';
-- ^ No composite analytical indexes — those belong on read models

-- READ MODEL 1: Real-time operational dashboard (hot tier)
-- Purpose: "what is happening right now?"
SELECT pgtrickle.create_stream_table(
    'order_dashboard',
    $$SELECT
        date_trunc('hour', created_at)     AS hour,
        region,
        status,
        COUNT(*)                           AS order_count,
        SUM(amount)                        AS total_amount,
        AVG(amount)                        AS avg_order_value,
        MAX(amount)                        AS largest_order
      FROM orders
      WHERE created_at > now() - '24 hours'::interval
      GROUP BY 1, 2, 3$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('order_dashboard', tier => 'hot');

-- READ MODEL 2: Customer 360 view (warm tier)
-- Purpose: "what do I know about this customer?"
-- Combines orders + customers in one pre-joined, flat table
SELECT pgtrickle.create_stream_table(
    'customer_360',
    $$SELECT
        c.id                               AS customer_id,
        c.name,
        c.email,
        c.tier,
        c.region                           AS customer_region,
        COUNT(o.id)                        AS total_orders,
        COALESCE(SUM(o.amount), 0)         AS lifetime_value,
        COALESCE(AVG(o.amount), 0)         AS avg_order_value,
        MAX(o.created_at)                  AS last_order_at,
        COALESCE(SUM(CASE WHEN o.status = 'returned'
                     THEN 1 ELSE 0 END), 0) AS return_count
      FROM customers c
      LEFT JOIN orders o ON o.customer_id = c.id
      GROUP BY c.id, c.name, c.email, c.tier, c.region$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('customer_360', tier => 'warm');

-- READ MODEL 3: Settlement reporting (cold tier)
-- Purpose: "what do the books say at end of day?"
SELECT pgtrickle.create_stream_table(
    'settlement_report',
    $$SELECT
        date_trunc('day', created_at)      AS settlement_day,
        region,
        COUNT(*) FILTER (WHERE status = 'completed') AS settled_orders,
        SUM(amount) FILTER (WHERE status = 'completed') AS settled_amount,
        COUNT(*) FILTER (WHERE status = 'refunded')  AS refund_count,
        SUM(amount) FILTER (WHERE status = 'refunded') AS refund_amount
      FROM orders
      GROUP BY 1, 2$$,
    schedule => '5m',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('settlement_report', tier => 'cold');

-- READ MODEL 4: Fraud signals (IMMEDIATE mode)
-- Purpose: "is this transaction suspicious RIGHT NOW?"
SELECT pgtrickle.create_stream_table(
    'high_value_recent_orders',
    $$SELECT
        customer_id,
        SUM(amount)   AS recent_total,
        COUNT(*)      AS recent_count,
        MAX(amount)   AS max_single_order
      FROM orders
      WHERE created_at > now() - '1 hour'::interval
        AND status != 'cancelled'
      GROUP BY customer_id
      HAVING SUM(amount) > 10000$$,
    schedule => 'IMMEDIATE',
    refresh_mode => 'DIFFERENTIAL'
);
```

The application's read path now never touches the `orders` or `customers`
tables directly:

```sql
-- Dashboard query: reads pre-computed stream table (< 1ms)
SELECT hour, region, order_count, total_amount
FROM order_dashboard
ORDER BY hour DESC, total_amount DESC;

-- Customer profile: reads pre-joined stream table (single row lookup)
SELECT * FROM customer_360 WHERE customer_id = 42;

-- Fraud check: reads IMMEDIATE-mode stream table (zero lag)
SELECT recent_total, recent_count
FROM high_value_recent_orders
WHERE customer_id = $1;
```

### Approach 2: Multi-Service CQRS (Microservices)

In a microservice architecture, multiple services share a PostgreSQL cluster
(or separate databases with cross-DB reads via `postgres_fdw`). Each service's
read model needs data from other services.

```sql
-- Order service owns the orders table
-- Fulfillment service owns the shipments table
-- Billing service reads from both — CQRS solves the cross-service join

-- Using postgres_fdw for cross-database read models
CREATE EXTENSION IF NOT EXISTS postgres_fdw;

CREATE SERVER fulfillment_db
    FOREIGN DATA WRAPPER postgres_fdw
    OPTIONS (host 'fulfillment-db.internal', dbname 'fulfillment', port '5432');

CREATE USER MAPPING FOR CURRENT_USER
    SERVER fulfillment_db OPTIONS (user 'readonly', password '...');

CREATE FOREIGN TABLE shipments_remote (
    order_id     BIGINT NOT NULL,
    tracking_no  TEXT,
    shipped_at   TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    carrier      TEXT
) SERVER fulfillment_db
  OPTIONS (table_name 'shipments');

-- Billing service read model: order + shipment status (denormalized join)
SELECT pgtrickle.create_stream_table(
    'order_fulfillment_view',
    $$SELECT
        o.id                  AS order_id,
        o.customer_id,
        o.amount,
        o.status              AS order_status,
        o.created_at,
        s.tracking_no,
        s.shipped_at,
        s.delivered_at,
        s.carrier,
        CASE
          WHEN s.delivered_at IS NOT NULL THEN 'delivered'
          WHEN s.shipped_at   IS NOT NULL THEN 'in_transit'
          WHEN o.status = 'confirmed'     THEN 'awaiting_shipment'
          ELSE o.status
        END AS combined_status
      FROM orders o
      LEFT JOIN shipments_remote s ON s.order_id = o.id$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
```

**Note:** When querying foreign tables, pg_trickle uses FULL refresh for the
foreign table join portion; the local table delta is still DIFFERENTIAL. This
is a known limitation with FDW sources.

### Approach 3: Event Sourcing + CQRS (Combined Pattern)

Event Sourcing stores every state change as an immutable event. CQRS provides
the read models derived from the event stream. Together, they are extremely
powerful. pg_trickle acts as the automatic projection engine.

```sql
-- Event store: append-only, never UPDATE or DELETE
CREATE TABLE domain_events (
    event_id      BIGSERIAL PRIMARY KEY,
    aggregate_id  UUID NOT NULL,
    aggregate_type TEXT NOT NULL,            -- 'Order', 'Customer', etc.
    event_type    TEXT NOT NULL,             -- 'OrderPlaced', 'OrderShipped', etc.
    event_version INT NOT NULL DEFAULT 1,
    payload       JSONB NOT NULL,
    metadata      JSONB NOT NULL DEFAULT '{}',  -- correlation_id, user_id, etc.
    occurred_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for per-aggregate event log replay
CREATE INDEX ON domain_events (aggregate_id, occurred_at);
CREATE INDEX ON domain_events (aggregate_type, event_type);

-- PROJECTION 1: Current order state (latest event per order)
SELECT pgtrickle.create_stream_table(
    'order_state',
    $$SELECT DISTINCT ON (aggregate_id)
        aggregate_id                            AS order_id,
        event_type                              AS last_event,
        (payload->>'status')                    AS status,
        (payload->>'amount')::numeric           AS amount,
        (payload->>'customer_id')::int          AS customer_id,
        occurred_at                             AS last_updated_at
      FROM domain_events
      WHERE aggregate_type = 'Order'
      ORDER BY aggregate_id, occurred_at DESC$$,
    schedule => '2s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true    -- Event store is append-only; enables faster deltas
);

-- PROJECTION 2: Customer order history (multiple events aggregated)
SELECT pgtrickle.create_stream_table(
    'customer_order_history',
    $$SELECT
        (payload->>'customer_id')::int AS customer_id,
        COUNT(*) FILTER (WHERE event_type = 'OrderPlaced')    AS total_orders,
        COUNT(*) FILTER (WHERE event_type = 'OrderCancelled') AS cancelled_orders,
        SUM((payload->>'amount')::numeric) FILTER
            (WHERE event_type = 'OrderPlaced')                AS total_spent,
        MAX(occurred_at) FILTER
            (WHERE event_type = 'OrderPlaced')                AS last_order_at
      FROM domain_events
      WHERE aggregate_type = 'Order'
      GROUP BY 1$$,
    schedule => '10s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);

-- PROJECTION 3: Audit trail (full event log per aggregate — temporal)
SELECT pgtrickle.create_stream_table(
    'order_audit_trail',
    $$SELECT
        aggregate_id    AS order_id,
        event_type,
        event_version,
        payload,
        metadata->>'user_id'          AS initiated_by,
        metadata->>'correlation_id'   AS correlation_id,
        occurred_at
      FROM domain_events
      WHERE aggregate_type = 'Order'
      ORDER BY aggregate_id, occurred_at$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL',
    append_only => true
);
```

**Why `append_only => true` matters here:**
The event store is never updated or deleted. Setting `append_only => true` tells
pg_trickle's CDC to skip delete-tracking in the change buffer, halving the
write overhead for projections.

---

## Read Model Design Strategies

### Strategy A: Operational Read Models (Sub-Second Freshness)

For data that drives real-time decisions: dashboards, fraud signals, SLA
monitoring, live inventory levels.

```sql
-- Pattern: narrow projection, tight time window, IMMEDIATE or 1s schedule
SELECT pgtrickle.create_stream_table(
    'live_inventory',
    $$SELECT
        product_id,
        SUM(quantity_in)  - SUM(quantity_out) AS available_units,
        MIN(warehouse_id) FILTER (WHERE quantity_in - quantity_out > 0)
                                              AS nearest_warehouse
      FROM inventory_movements
      WHERE movement_date > CURRENT_DATE - 7
      GROUP BY product_id
      HAVING SUM(quantity_in) - SUM(quantity_out) >= 0$$,
    schedule => '2s',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('live_inventory', tier => 'hot');
```

**Design rules:**
- Keep the time window tight (last N hours/days, not all time).
- Avoid `MIN`/`MAX` aggregates when possible — they require GROUP_RESCAN.
- Use `HAVING` rather than post-query filtering to reduce output rows.

### Strategy B: Analytics Read Models (Aggregated)

For data that drives reporting, BI tools, ML training data pipelines.

```sql
-- Pattern: wide time window, heavy aggregation, 1m+ schedule, cold/warm tier
SELECT pgtrickle.create_stream_table(
    'product_performance_monthly',
    $$SELECT
        date_trunc('month', o.created_at)      AS month,
        p.category,
        p.brand,
        COUNT(o.id)                            AS units_sold,
        SUM(o.amount)                          AS gross_revenue,
        AVG(o.amount)                          AS avg_selling_price,
        COUNT(DISTINCT o.customer_id)          AS unique_buyers,
        COUNT(o.id) FILTER (WHERE o.status = 'returned')
                                               AS return_count,
        ROUND(
          COUNT(o.id) FILTER (WHERE o.status = 'returned')::numeric
          / NULLIF(COUNT(o.id), 0) * 100, 2
        )                                      AS return_rate_pct
      FROM orders o
      JOIN products p ON p.id = o.product_id
      WHERE o.status IN ('completed', 'returned')
      GROUP BY 1, 2, 3$$,
    schedule => '10m',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('product_performance_monthly', tier => 'warm');
```

**Design rules:**
- Use `date_trunc` for time bucketing — it creates stable group keys that
  DIFFERENTIAL can efficiently update when new data arrives.
- `COUNT(DISTINCT ...)` is supported as a DIFFERENTIAL aggregate in pg_trickle
  v0.14+.
- Chain from operational read models when the analytics model can be derived
  from an existing projection:

```sql
-- Analytics model chaining from operational model
SELECT pgtrickle.create_stream_table(
    'weekly_dashboard_rollup',
    $$SELECT
        date_trunc('week', hour) AS week,
        region,
        SUM(order_count)  AS weekly_orders,
        SUM(total_amount) AS weekly_revenue
      FROM order_dashboard    -- <- reads from another stream table
      GROUP BY 1, 2$$,
    schedule => 'calculated',     -- <- auto-refreshes when order_dashboard updates
    refresh_mode => 'DIFFERENTIAL'
);
```

### Strategy C: Cross-Service Read Models (Denormalized Joins)

For queries that span conceptual "service boundaries" within a single database.
The join happens at projection time (once, cheaply) rather than at query time
(repeatedly, expensively).

```sql
-- "Order enrichment" view: pre-joins 5 tables into one flat read model
SELECT pgtrickle.create_stream_table(
    'enriched_orders',
    $$SELECT
        o.id                      AS order_id,
        o.created_at,
        o.status,
        o.amount,
        -- Customer dimension (denormalized into the row)
        c.id                      AS customer_id,
        c.name                    AS customer_name,
        c.tier                    AS customer_tier,
        c.region                  AS customer_region,
        -- Product dimension
        p.id                      AS product_id,
        p.name                    AS product_name,
        p.category                AS product_category,
        p.brand                   AS product_brand,
        -- Promotion (nullable)
        pr.code                   AS promo_code,
        pr.discount_pct           AS discount_applied,
        -- Computed fields
        o.amount * (1 - COALESCE(pr.discount_pct, 0) / 100.0)
                                  AS net_amount
      FROM orders o
      JOIN customers c   ON c.id = o.customer_id
      JOIN products p    ON p.id = o.product_id
      LEFT JOIN promotions pr ON pr.order_id = o.id$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Now any service can query the enriched view without knowing the schema
SELECT order_id, customer_name, product_category, net_amount
FROM enriched_orders
WHERE customer_tier = 'premium'
  AND product_category = 'electronics'
  AND created_at > now() - '7 days'::interval;
-- One table scan, zero joins, sub-millisecond response
```

---

## Worked Example: E-Commerce Order Service

A realistic end-to-end CQRS deployment for an e-commerce platform.

```sql
-- ── COMMAND SIDE ─────────────────────────────────────────────────────────────

CREATE TABLE customers (
    id         SERIAL PRIMARY KEY,
    name       TEXT NOT NULL,
    email      TEXT UNIQUE NOT NULL,
    tier       TEXT NOT NULL DEFAULT 'standard',  -- standard, premium, vip
    region     TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE products (
    id          SERIAL PRIMARY KEY,
    name        TEXT NOT NULL,
    category    TEXT NOT NULL,
    brand       TEXT NOT NULL,
    base_price  NUMERIC(10, 2) NOT NULL,
    active      BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE orders (
    id          BIGSERIAL PRIMARY KEY,
    customer_id INT NOT NULL REFERENCES customers(id),
    product_id  INT NOT NULL REFERENCES products(id),
    quantity    INT NOT NULL DEFAULT 1,
    amount      NUMERIC(12, 2) NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── READ MODELS ───────────────────────────────────────────────────────────────

-- 1. Product catalog with live inventory context
SELECT pgtrickle.create_stream_table(
    'product_catalog',
    $$SELECT
        p.id,
        p.name,
        p.category,
        p.brand,
        p.base_price,
        COUNT(o.id) FILTER (WHERE o.created_at > now() - '7 days'::interval
                             AND o.status != 'cancelled')
                                  AS units_sold_7d,
        COUNT(DISTINCT o.customer_id) FILTER (
            WHERE o.created_at > now() - '30 days'::interval)
                                  AS buyers_30d
      FROM products p
      LEFT JOIN orders o ON o.product_id = p.id
      WHERE p.active = true
      GROUP BY p.id, p.name, p.category, p.brand, p.base_price$$,
    schedule => '1m',
    refresh_mode => 'DIFFERENTIAL'
);

-- 2. Customer 360 (for customer service, personalisation engines)
SELECT pgtrickle.create_stream_table(
    'customer_profile',
    $$SELECT
        c.id                      AS customer_id,
        c.name,
        c.email,
        c.tier,
        c.region,
        COUNT(o.id)               AS total_orders,
        COALESCE(SUM(o.amount), 0) AS lifetime_value,
        COALESCE(AVG(o.amount), 0) AS avg_order_value,
        MAX(o.created_at)          AS last_order_at,
        COALESCE(SUM(o.amount) FILTER (
            WHERE o.created_at > now() - '90 days'::interval), 0)
                                  AS spend_90d
      FROM customers c
      LEFT JOIN orders o ON o.customer_id = c.id
        AND o.status IN ('completed', 'shipped')
      GROUP BY c.id, c.name, c.email, c.tier, c.region$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);

-- 3. Real-time revenue dashboard (for ops team)
SELECT pgtrickle.create_stream_table(
    'revenue_dashboard',
    $$SELECT
        date_trunc('minute', created_at) AS minute,
        region,
        status,
        COUNT(*)                         AS order_count,
        SUM(amount)                      AS revenue,
        AVG(amount)                      AS avg_basket
      FROM orders
      WHERE created_at > now() - '2 hours'::interval
      GROUP BY 1, 2, 3$$,
    schedule => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
SELECT pgtrickle.alter_stream_table('revenue_dashboard', tier => 'hot');

-- 4. Fraud / anomaly signals (IMMEDIATE — fires on every order write)
SELECT pgtrickle.create_stream_table(
    'customer_velocity',
    $$SELECT
        customer_id,
        COUNT(*)      AS orders_1h,
        SUM(amount)   AS spend_1h
      FROM orders
      WHERE created_at > now() - '1 hour'::interval
        AND status != 'cancelled'
      GROUP BY customer_id
      HAVING COUNT(*) > 5 OR SUM(amount) > 5000$$,
    schedule => 'IMMEDIATE',
    refresh_mode => 'DIFFERENTIAL'
);

-- ── APPLICATION PATTERNS ──────────────────────────────────────────────────────

-- Write path: touches only base tables
-- CREATE ORDER command:
BEGIN;
  INSERT INTO orders (customer_id, product_id, quantity, amount, status)
  VALUES ($customer_id, $product_id, $quantity, $amount, 'pending');
COMMIT;
-- pg_trickle automatically propagates the change to all read models

-- Read paths: never touch base tables
-- "Show me this customer's profile" — from customer_profile stream table
SELECT * FROM customer_profile WHERE customer_id = $customer_id;

-- "Is this customer flagged for fraud?" — from IMMEDIATE stream table
SELECT orders_1h, spend_1h FROM customer_velocity WHERE customer_id = $customer_id;

-- "What's our revenue in the last 30 minutes?" — from revenue_dashboard
SELECT SUM(revenue) FROM revenue_dashboard
WHERE minute > now() - '30 minutes'::interval;
```

---

## Read Model Versioning

When a query definition must change (new columns, changed logic), the old
stream table continues serving reads while the new version builds.

```sql
-- Step 1: Create the new version alongside the old one
SELECT pgtrickle.create_stream_table(
    'customer_profile_v2',
    $$SELECT
        c.id                       AS customer_id,
        c.name,
        c.email,
        c.tier,
        c.region,
        COUNT(o.id)                AS total_orders,
        COALESCE(SUM(o.amount), 0) AS lifetime_value,
        -- NEW: NPS score join
        AVG(n.score)               AS avg_nps_score,
        COUNT(n.id)                AS nps_responses
      FROM customers c
      LEFT JOIN orders o   ON o.customer_id = c.id
        AND o.status IN ('completed', 'shipped')
      LEFT JOIN nps_surveys n ON n.customer_id = c.id
      GROUP BY c.id, c.name, c.email, c.tier, c.region$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Step 2: Verify v2 data looks correct (compare a sample)
SELECT v1.customer_id, v1.total_orders, v2.total_orders, v2.avg_nps_score
FROM customer_profile v1
JOIN customer_profile_v2 v2 USING (customer_id)
WHERE v1.total_orders != v2.total_orders
LIMIT 10;

-- Step 3: Switch application traffic to v2
-- (deploy app config change pointing to customer_profile_v2)

-- Step 4: Drop v1
SELECT pgtrickle.drop_stream_table('customer_profile');
```

---

## Consistency Trade-Offs

CQRS with DIFFERENTIAL refresh introduces a **consistency lag** — the read
model is slightly behind the write model. Choose the right mode for each
read model:

| Use Case                              | Schedule        | Max Lag     | Notes                          |
|---------------------------------------|-----------------|-------------|--------------------------------|
| Fraud detection, balance checks       | `IMMEDIATE`     | 0ms         | Adds write-path latency        |
| Operational dashboards                | `1s` – `5s`     | 1–5s        | Amortizes costs, still "live"  |
| Customer profiles, product catalog    | `10s` – `1m`    | 10s–1m      | Invisible to users at this lag |
| Analytics reports, BI tooling         | `5m` – `30m`    | 5–30m       | Expected for batch analytics   |
| Daily settlement, ledger close        | `1h` (or cron)  | Up to 1h    | Scheduled cold-tier read model |

**When exact consistency is required:** Use IMMEDIATE mode for the specific
read model. The rest can remain eventual. Not every read model needs the
same consistency guarantee.

```sql
-- Override specific read models that require stronger consistency
SELECT pgtrickle.alter_stream_table('account_balances',
    schedule => 'IMMEDIATE',
    refresh_mode => 'DIFFERENTIAL'
);

-- Relax read models where staleness is acceptable
SELECT pgtrickle.alter_stream_table('product_catalog',
    schedule => '2m'
);
```

---

## Complementary PostgreSQL Extensions

### pgvector — Semantic Read Models

[pgvector](https://github.com/pgvector/pgvector) stores embedding vectors for
semantic search. A stream table can materialize the inputs for embedding
computation, and `pgvector` handles the ANN index.

```sql
-- Stream table materializes the text content to embed
SELECT pgtrickle.create_stream_table(
    'product_search_inputs',
    $$SELECT id AS product_id,
             name || ' ' || category || ' ' || brand AS search_text
      FROM products WHERE active = true$$,
    schedule => '5m',
    refresh_mode => 'DIFFERENTIAL'
);
-- An application worker then calls an embedding API for changed rows
-- and updates a pgvector table with the new embeddings
```

### pg_cron — Time-Triggered Projections

[pg_cron](https://github.com/citusdata/pg_cron) can trigger cold-tier read
model refreshes on a calendar schedule (e.g., end-of-month settlement).

```sql
-- Schedule a monthly settlement projection refresh
SELECT cron.schedule(
    'monthly-settlement-refresh',
    '0 0 1 * *',  -- First day of each month at midnight
    $$SELECT pgtrickle.refresh_stream_table('monthly_settlement', force => true)$$
);
```

### pgmq — Command Queue

[pgmq](https://github.com/tembo-io/pgmq) implements a durable message queue
in PostgreSQL. Commands can arrive via pgmq and be applied to the write model,
keeping the full command bus inside the database.

```sql
CREATE EXTENSION IF NOT EXISTS pgmq;

-- Commands arrive as messages in a queue
SELECT pgmq.create('order_commands');

-- A command processor reads and applies commands
-- SELECT * FROM pgmq.read('order_commands', 30, 10);
-- ... apply each command to orders table ...
-- SELECT pgmq.delete('order_commands', msg_id);
```

---

## Potential pg_trickle Extensions

### Extension 1: Read Model Registry

A system catalog table that records which stream tables are acting as CQRS
read models, their upstream command-side sources, and their freshness SLAs.

```sql
-- Proposed API
SELECT pgtrickle.register_read_model(
    stream_table => 'customer_profile',
    command_sources => ARRAY['customers', 'orders'],
    max_lag_seconds => 30,
    description => 'Customer 360 profile for personalisation API'
);

-- Query the registry
SELECT * FROM pgtrickle.read_model_registry;
-- pgt_name | command_sources | max_lag_s | current_lag_s | sla_ok
```

### Extension 2: Staleness Monitoring per Read Model

Alerting when a read model exceeds its configured freshness SLA.

```sql
-- Check all read models against their SLA
SELECT pgt_name, max_lag_s, current_lag_s,
       current_lag_s > max_lag_s AS sla_breached
FROM pgtrickle.read_model_sla_status
WHERE current_lag_s > max_lag_s;
```

---

## Observability & Monitoring

```sql
-- Read model freshness: how stale are each read model's results?
SELECT
    pgt_name,
    refresh_mode,
    last_refresh_at,
    EXTRACT(EPOCH FROM (now() - last_refresh_at)) AS age_seconds,
    avg_refresh_duration_ms,
    total_refreshes,
    diff_speedup
FROM pgtrickle.stream_tables_info
ORDER BY age_seconds DESC;

-- Read model efficiency: how much work is being done vs. skipped?
SELECT
    pgt_name,
    avg_change_ratio,
    diff_speedup,
    CASE
      WHEN diff_speedup < 1.5 THEN 'consider FULL refresh'
      WHEN avg_change_ratio > 0.5 THEN 'high change ratio — check schedule'
      ELSE 'ok'
    END AS recommendation
FROM pgtrickle.refresh_efficiency();

-- Identify unused read models (no reads in the last 24h)
SELECT pgt_name, last_refresh_at
FROM pgtrickle.stream_tables_info
WHERE last_refresh_at < now() - '24 hours'::interval;
```

---

## Testing Strategies

### Unit Testing: Command Side

Test the write model in isolation. Commands should be simple inserts with
referential integrity constraints — easy to test.

```sql
-- Test: order amount must be positive
DO $$
BEGIN
  PERFORM id FROM orders WHERE id = (
    SELECT id FROM orders
    ORDER BY id DESC LIMIT 1
  );
  -- Insert a valid order
  INSERT INTO orders (customer_id, product_id, amount, status)
  VALUES (1, 1, 50.00, 'pending');
  ASSERT FOUND, 'Order should have been inserted';
END;
$$;
```

### Integration Testing: Read Model Consistency

After a command, assert the read model updates within the expected lag.

```python
import time
import psycopg2

def test_customer_profile_updates_after_order():
    conn = psycopg2.connect("...")
    cur = conn.cursor()

    # Command: place an order
    cur.execute("""
        INSERT INTO orders (customer_id, product_id, amount, status)
        VALUES (42, 1, 99.99, 'completed')
    """)
    conn.commit()

    # Wait for read model to refresh (schedule is 30s, test waits 35s)
    time.sleep(35)

    # Assert read model reflects the command
    cur.execute("""
        SELECT total_orders, lifetime_value
        FROM customer_profile
        WHERE customer_id = 42
    """)
    profile = cur.fetchone()
    assert profile is not None
    assert profile[1] >= 99.99, f"Expected lifetime_value >= 99.99, got {profile[1]}"
```

### Property Testing: Read Model Correctness

The read model should always agree with a fresh query on the source tables.

```sql
-- Verification query: does the stream table match a fresh computation?
WITH fresh AS (
    SELECT customer_id, COUNT(*) AS total_orders, SUM(amount) AS lifetime_value
    FROM orders
    WHERE status IN ('completed', 'shipped')
    GROUP BY customer_id
)
SELECT cp.customer_id,
       cp.total_orders   AS cached,  fresh.total_orders  AS computed,
       cp.lifetime_value AS cached,  fresh.lifetime_value AS computed
FROM customer_profile cp
JOIN fresh USING (customer_id)
WHERE cp.total_orders != fresh.total_orders
   OR abs(cp.lifetime_value - fresh.lifetime_value) > 0.01;
-- Zero rows expected
```

---

## Security Considerations

```sql
-- Application roles: command-side write access only
CREATE ROLE order_service_writer;
GRANT INSERT, UPDATE ON orders TO order_service_writer;
REVOKE SELECT ON orders FROM order_service_writer;  -- Can't read source table
GRANT SELECT ON order_dashboard TO order_service_writer;  -- Only reads stream tables

-- BI / analytics roles: read-only, stream tables only
CREATE ROLE analytics_reader;
GRANT SELECT ON customer_profile TO analytics_reader;
GRANT SELECT ON product_performance_monthly TO analytics_reader;
GRANT SELECT ON settlement_report TO analytics_reader;
REVOKE SELECT ON orders, customers, products FROM analytics_reader;

-- Sensitive fields: mask PII in the read model at projection time
SELECT pgtrickle.create_stream_table(
    'customer_profile_masked',
    $$SELECT
        id                        AS customer_id,
        left(name, 1) || '***'   AS name_masked,
        regexp_replace(email, '(.).+@', '\1***@') AS email_masked,
        tier,
        region
      FROM customers$$,
    schedule => '30s',
    refresh_mode => 'DIFFERENTIAL'
);
-- Give external BI tools access only to the masked read model
```

---

## Cost Analysis

| Configuration                  | Write Overhead  | Read Model Latency | CDC Cost       |
|--------------------------------|-----------------|-------------------|----------------|
| No CQRS                        | Baseline        | Slow (ad-hoc SQL) | None           |
| CQRS, 1 read model, DIFF       | +2% CDC trigger | 1–30s (schedule)  | One change buf |
| CQRS, 5 read models, DIFF      | +2% CDC trigger | 1–30s             | One change buf |
| CQRS, 10 read models, DIFF     | +2% CDC trigger | 1–30s             | One change buf |
| CQRS, 1 read model, IMMEDIATE  | +15–30% (sync)  | 0ms               | One change buf |

**Key insight:** Multiple read models sharing the same source table pay the
CDC overhead only once. Adding the 5th read model to a source table is nearly
free — just the incremental DIFFERENTIAL computation cost.

---

## Comparison with Traditional Approaches

| Approach                    | Complexity | Read Perf | Write Perf | Freshness  | pg_trickle Advantage                  |
|-----------------------------|------------|-----------|------------|------------|---------------------------------------|
| Ad-hoc complex queries      | Low        | Slow      | Fast       | Always     | None (pg_trickle replaces these)      |
| Indexed materialized views  | Medium     | Fast      | Slower     | Manual     | Auto-refresh, DIFFERENTIAL updates    |
| Application-level caching   | High       | Fast      | Fast       | Cache TTL  | No cache invalidation bugs            |
| Redis projection store      | Very High  | Fast      | Fast       | Eventual   | No external dep, SQL joins, ACID      |
| Separate analytics DB       | Very High  | Fast      | Fast       | Batch lag  | No ETL pipeline, no infra overhead    |
| **pg_trickle CQRS**         | **Low**    | **Fast**  | **Fast**   | **1s–1m**  | **One tool, pure SQL, auto-refresh**  |

---

## When NOT to Use CQRS

CQRS is not always the right choice. Avoid it when:

- **Your reads and writes share the same shape.** If a CRUD admin panel reads
  and writes the same row, a stream table adds complexity without benefit.
- **Exact real-time consistency is required for all reads.** IMMEDIATE mode
  helps, but if every single query needs the freshest data within a single
  transaction, a direct query on the source is simpler.
- **Your team is small and the domain is simple.** CQRS adds two layers (write
  model + read models) to reason about. In a 2-table application, the overhead
  isn't worth it.
- **You have a single read pattern.** If all queries look the same, one
  materialized view (or standard pg_trickle stream table) is sufficient.
  Full CQRS is for systems with many diverse read models.

---

## References

- Martin Fowler, "CQRS" — https://martinfowler.com/bliki/CQRS.html
- Greg Young, "CQRS Documents" — https://cqrs.files.wordpress.com/2010/11/cqrs_documents.pdf
- Udi Dahan, "Clarified CQRS" — https://udidahan.com/2009/12/09/clarified-cqrs/
- Microsoft Azure Architecture Guide: CQRS pattern — https://learn.microsoft.com/en-us/azure/architecture/patterns/cqrs
- Martin Fowler, "Event Sourcing" — https://martinfowler.com/eaaDev/EventSourcing.html
- pg_trickle SQL Reference — [SQL_REFERENCE.md](../docs/SQL_REFERENCE.md)
- pg_trickle DVM Operators — [DVM_OPERATORS.md](../docs/DVM_OPERATORS.md)
- pgvector — https://github.com/pgvector/pgvector
- pgmq — https://github.com/tembo-io/pgmq
- pg_cron — https://github.com/citusdata/pg_cron
