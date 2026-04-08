-- =============================================================================
-- pg_trickle playground — seed data
--
-- Creates sample tables and stream tables for exploring pg_trickle features.
-- This file is automatically executed when the playground container starts.
-- =============================================================================

-- Enable the extension
CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- ── Base tables ─────────────────────────────────────────────────────────────

CREATE TABLE customers (
    id      SERIAL PRIMARY KEY,
    name    TEXT        NOT NULL,
    region  TEXT        NOT NULL,
    email   TEXT        NOT NULL
);

CREATE TABLE products (
    id       SERIAL PRIMARY KEY,
    name     TEXT           NOT NULL,
    category TEXT           NOT NULL,
    price    NUMERIC(10,2)  NOT NULL
);

CREATE TABLE orders (
    id          SERIAL PRIMARY KEY,
    customer_id INT            NOT NULL REFERENCES customers(id),
    product_id  INT            NOT NULL REFERENCES products(id),
    quantity    INT            NOT NULL DEFAULT 1,
    order_date  DATE           NOT NULL DEFAULT CURRENT_DATE
);

-- ── Seed data ───────────────────────────────────────────────────────────────

INSERT INTO customers (name, region, email) VALUES
    ('Alice Johnson',   'North America', 'alice@example.com'),
    ('Bob Smith',       'Europe',        'bob@example.com'),
    ('Charlie Lee',     'Asia Pacific',  'charlie@example.com'),
    ('Diana Martinez',  'North America', 'diana@example.com'),
    ('Eve Brown',       'Europe',        'eve@example.com'),
    ('Frank Wilson',    'Asia Pacific',  'frank@example.com'),
    ('Grace Kim',       'North America', 'grace@example.com'),
    ('Hank Davis',      'Europe',        'hank@example.com');

INSERT INTO products (name, category, price) VALUES
    ('Widget',          'Hardware',     29.99),
    ('Gadget',          'Hardware',     49.99),
    ('Thingamajig',     'Hardware',     19.99),
    ('Pro License',     'Software',    199.99),
    ('Basic License',   'Software',     99.99),
    ('Support Plan',    'Services',    149.99),
    ('Training Pack',   'Services',     79.99),
    ('Data Connector',  'Software',     59.99);

INSERT INTO orders (customer_id, product_id, quantity, order_date) VALUES
    (1, 1,  3, CURRENT_DATE - INTERVAL '30 days'),
    (1, 4,  1, CURRENT_DATE - INTERVAL '25 days'),
    (2, 2,  2, CURRENT_DATE - INTERVAL '20 days'),
    (2, 5,  1, CURRENT_DATE - INTERVAL '18 days'),
    (3, 1,  5, CURRENT_DATE - INTERVAL '15 days'),
    (3, 6,  1, CURRENT_DATE - INTERVAL '12 days'),
    (4, 3, 10, CURRENT_DATE - INTERVAL '10 days'),
    (4, 7,  2, CURRENT_DATE - INTERVAL '8 days'),
    (5, 4,  1, CURRENT_DATE - INTERVAL '7 days'),
    (5, 8,  3, CURRENT_DATE - INTERVAL '5 days'),
    (6, 1,  4, CURRENT_DATE - INTERVAL '4 days'),
    (6, 2,  2, CURRENT_DATE - INTERVAL '3 days'),
    (7, 5,  1, CURRENT_DATE - INTERVAL '2 days'),
    (7, 6,  1, CURRENT_DATE - INTERVAL '1 day'),
    (8, 3,  6, CURRENT_DATE),
    (8, 7,  1, CURRENT_DATE),
    (1, 2,  2, CURRENT_DATE),
    (3, 4,  1, CURRENT_DATE),
    (5, 1,  3, CURRENT_DATE);

-- ── Stream tables ───────────────────────────────────────────────────────────

-- 1. Sales by region — basic aggregate
SELECT pgtrickle.create_stream_table(
    name     => 'sales_by_region',
    query    => $$
        SELECT c.region,
               COUNT(*)                          AS order_count,
               SUM(o.quantity * p.price)          AS total_revenue,
               ROUND(AVG(o.quantity * p.price), 2) AS avg_order_value
        FROM orders o
        JOIN customers c ON c.id = o.customer_id
        JOIN products p  ON p.id = o.product_id
        GROUP BY c.region
    $$,
    schedule => '1s'
);

-- 2. Top products — window function (RANK)
SELECT pgtrickle.create_stream_table(
    name     => 'top_products',
    query    => $$
        SELECT p.category,
               p.name AS product_name,
               SUM(o.quantity) AS total_sold,
               RANK() OVER (PARTITION BY category ORDER BY SUM(o.quantity) DESC) AS rnk
        FROM orders o
        JOIN products p ON p.id = o.product_id
        GROUP BY p.category, p.name
    $$,
    schedule => '2s'
);

-- 3. Customer lifetime value — multi-table join + aggregates
SELECT pgtrickle.create_stream_table(
    name     => 'customer_lifetime_value',
    query    => $$
        SELECT c.id AS customer_id,
               c.name,
               c.region,
               COUNT(DISTINCT o.id)              AS order_count,
               SUM(o.quantity)                    AS total_items,
               SUM(o.quantity * p.price)          AS total_revenue,
               MIN(o.order_date)                  AS first_order,
               MAX(o.order_date)                  AS last_order
        FROM customers c
        JOIN orders o   ON o.customer_id = c.id
        JOIN products p ON p.id = o.product_id
        GROUP BY c.id, c.name, c.region
    $$,
    schedule => '2s'
);

-- 4. Daily revenue — time-series aggregation
SELECT pgtrickle.create_stream_table(
    name     => 'daily_revenue',
    query    => $$
        SELECT o.order_date,
               COUNT(*)                  AS num_orders,
               SUM(o.quantity * p.price) AS revenue
        FROM orders o
        JOIN products p ON p.id = o.product_id
        GROUP BY o.order_date
    $$,
    schedule => '1s'
);

-- 5. Active products — EXISTS subquery
SELECT pgtrickle.create_stream_table(
    name     => 'active_products',
    query    => $$
        SELECT p.id, p.name, p.category, p.price
        FROM products p
        WHERE EXISTS (SELECT 1 FROM orders o WHERE o.product_id = p.id)
    $$,
    schedule => '2s'
);
