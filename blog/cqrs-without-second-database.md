[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# CQRS Without a Second Database

## Command Query Responsibility Segregation using stream tables instead of a separate read store

---

CQRS is a clean idea with an ugly implementation.

The pattern says: separate your write model (normalized, optimized for transactions) from your read model (denormalized, optimized for queries). Writes go to one place, reads from another.

The standard implementation: writes go to PostgreSQL, a CDC pipeline (Debezium, usually) reads the WAL, transforms the events, and writes them to a separate read store (Elasticsearch, DynamoDB, a read replica with different indexes, sometimes a second PostgreSQL instance with materialized views).

Now you're maintaining two databases, a CDC pipeline, a schema registry, retry logic, monitoring for the pipeline, and a runbook for when the pipeline breaks and your read model is 20 minutes behind.

pg_trickle collapses this into one PostgreSQL instance.

---

## The Architecture

```
Traditional CQRS:

  Application
    │  writes ─→  PostgreSQL (write model)
    │                   │
    │                   ▼
    │              Debezium CDC ──→ Kafka ──→ Consumer ──→ Elasticsearch (read model)
    │                                                          │
    └── reads ←────────────────────────────────────────────────┘


pg_trickle CQRS:

  Application
    │  writes ─→  PostgreSQL
    │                   │
    │              CDC triggers ──→ stream tables (read model)
    │                                    │
    └── reads ←──────────────────────────┘
```

The write model is your normalized tables. The read model is stream tables — denormalized projections that pg_trickle maintains automatically.

---

## A Concrete Example: Order Management

The write model is a standard normalized schema:

```sql
CREATE TABLE customers (
    id       bigint PRIMARY KEY,
    name     text NOT NULL,
    email    text NOT NULL,
    tier     text NOT NULL DEFAULT 'standard'
);

CREATE TABLE orders (
    id          bigserial PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES customers(id),
    status      text NOT NULL DEFAULT 'pending',
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE order_items (
    id          bigserial PRIMARY KEY,
    order_id    bigint NOT NULL REFERENCES orders(id),
    product_id  bigint NOT NULL,
    quantity    int NOT NULL,
    unit_price  numeric(10,2) NOT NULL
);

CREATE TABLE products (
    id       bigint PRIMARY KEY,
    name     text NOT NULL,
    category text NOT NULL
);
```

The application writes to these tables with normal INSERT/UPDATE/DELETE statements. The schema is normalized — no redundancy, no denormalization trade-offs.

### The Read Model

The API needs a flat "order detail" view that includes everything in one query: order info, customer name, line items with product names, and totals.

**Without pg_trickle**, this is either a complex JOIN query on every API request (slow at scale), or a denormalized table maintained by application code (error-prone and stale).

**With pg_trickle:**

```sql
-- Order detail view: everything the API needs in one row per order
SELECT pgtrickle.create_stream_table(
    'order_detail_view',
    $$SELECT
        o.id AS order_id,
        o.status,
        o.created_at,
        c.name AS customer_name,
        c.email AS customer_email,
        c.tier AS customer_tier,
        SUM(oi.quantity * oi.unit_price) AS order_total,
        COUNT(oi.id) AS line_item_count,
        array_agg(DISTINCT p.category) AS categories
      FROM orders o
      JOIN customers c ON c.id = o.customer_id
      JOIN order_items oi ON oi.order_id = o.id
      JOIN products p ON p.id = oi.product_id
      GROUP BY o.id, o.status, o.created_at,
               c.name, c.email, c.tier$$,
    refresh_mode => 'IMMEDIATE'
);
```

With `IMMEDIATE` mode, the stream table is updated in the same transaction as the write. The API reads from `order_detail_view` and gets a pre-joined, pre-aggregated result with zero lag.

---

## Read-Your-Writes

The critical property for CQRS in a web application is read-your-writes consistency. When a user places an order, the confirmation page should show that order immediately.

With a traditional CDC-based read model, there's a lag: the order is written, the CDC pipeline picks it up, transforms it, writes it to the read store. The confirmation page might not see the order for a few seconds.

With IMMEDIATE mode, the stream table is updated within the same PostgreSQL transaction. The API can read from the stream table in the same request that wrote the order:

```sql
BEGIN;
-- Write the order
INSERT INTO orders (customer_id) VALUES (42) RETURNING id;
-- order_id = 1001

-- Write line items
INSERT INTO order_items (order_id, product_id, quantity, unit_price)
VALUES (1001, 7, 2, 29.99), (1001, 12, 1, 49.99);

-- Read the fully-denormalized view — already reflects the new order
SELECT * FROM order_detail_view WHERE order_id = 1001;
COMMIT;
```

No polling. No retry. No "wait 2 seconds and then refresh the page."

---

## Multiple Read Models

Different API consumers need different projections. The customer portal needs order history. The warehouse needs picking lists. The finance team needs revenue summaries.

```sql
-- Customer's order history (for the customer portal)
SELECT pgtrickle.create_stream_table(
    'customer_order_history',
    $$SELECT
        c.id AS customer_id,
        c.name,
        COUNT(o.id) AS total_orders,
        SUM(oi.quantity * oi.unit_price) AS lifetime_spend,
        MAX(o.created_at) AS last_order_at
      FROM customers c
      LEFT JOIN orders o ON o.customer_id = c.id
      LEFT JOIN order_items oi ON oi.order_id = o.id
      GROUP BY c.id, c.name$$,
    refresh_mode => 'IMMEDIATE'
);

-- Picking list (for the warehouse)
SELECT pgtrickle.create_stream_table(
    'warehouse_picking_list',
    $$SELECT
        o.id AS order_id,
        p.name AS product_name,
        p.category,
        oi.quantity,
        o.created_at AS ordered_at
      FROM orders o
      JOIN order_items oi ON oi.order_id = o.id
      JOIN products p ON p.id = oi.product_id
      WHERE o.status = 'confirmed'$$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);

-- Revenue summary (for finance dashboards)
SELECT pgtrickle.create_stream_table(
    'revenue_summary',
    $$SELECT
        date_trunc('day', o.created_at) AS day,
        p.category,
        SUM(oi.quantity * oi.unit_price) AS revenue,
        COUNT(DISTINCT o.id) AS order_count
      FROM orders o
      JOIN order_items oi ON oi.order_id = o.id
      JOIN products p ON p.id = oi.product_id
      WHERE o.status IN ('confirmed', 'shipped', 'delivered')
      GROUP BY date_trunc('day', o.created_at), p.category$$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL'
);
```

Notice the mixed modes: the customer portal uses IMMEDIATE (read-your-writes matters), the warehouse uses DIFFERENTIAL with a 2-second schedule (a small delay is fine), the finance dashboard uses a 5-second schedule (freshness is nice but not critical).

Each read model is independently maintained. Adding a new one doesn't affect the others.

---

## What You Don't Need Anymore

With stream tables as your read model:

1. **No CDC pipeline.** No Debezium, no Kafka Connect, no custom consumers. Change capture is built into pg_trickle's triggers.

2. **No separate read database.** The read model lives in the same PostgreSQL instance. Same backup, same monitoring, same connection string.

3. **No schema synchronization.** When the write model schema changes, you update the stream table's query with `alter_stream_table`. No schema registry, no Avro evolution rules.

4. **No consistency reconciliation.** The read model is maintained transactionally. There's no need for periodic reconciliation jobs to fix drift between the write and read models.

5. **No read replica lag monitoring.** Stream tables don't use PostgreSQL replication. They're regular tables maintained by the same instance.

---

## When the Single-Database Approach Breaks Down

pg_trickle's CQRS works inside a single PostgreSQL instance (or Citus cluster). It breaks down when:

- **Read and write workloads need separate scaling.** If your reads need 10× the compute of your writes, you might want a separate read cluster. pg_trickle can still help here: use the outbox + relay to replicate stream table deltas to a read-only PostgreSQL replica.

- **The read model needs a different query engine.** If your read model is a full-text search index that needs inverted indexes with BM25 scoring, Elasticsearch is the right tool. (Though PostgreSQL's full-text search with GIN indexes is surprisingly capable.)

- **Terabyte-scale analytics.** If your read model is a columnar analytics store processing terabytes, a dedicated OLAP system (ClickHouse, DuckDB) is more appropriate.

For the vast majority of CQRS use cases — operational read models, denormalized API views, real-time dashboards — a single PostgreSQL instance with stream tables is simpler, cheaper, and more correct than the traditional multi-system architecture.
