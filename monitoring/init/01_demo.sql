-- pg_trickle demo init script
-- Loaded automatically by postgres on first start via /docker-entrypoint-initdb.d/

-- Install the extension.
CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- ── Demo schema ────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS orders (
    order_id   BIGSERIAL PRIMARY KEY,
    customer   TEXT NOT NULL,
    product    TEXT NOT NULL,
    amount     NUMERIC(10, 2) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS products (
    product_id SERIAL PRIMARY KEY,
    sku        TEXT UNIQUE NOT NULL,
    name       TEXT NOT NULL,
    price      NUMERIC(10, 2) NOT NULL
);

-- ── Stream tables ──────────────────────────────────────────────────────────

-- Daily sales totals per product
SELECT pgtrickle.create_stream_table(
    'public.daily_sales',
    $$
        SELECT
            product,
            date_trunc('day', created_at) AS day,
            count(*)                       AS order_count,
            sum(amount)                    AS total_amount
        FROM orders
        GROUP BY product, date_trunc('day', created_at)
    $$,
    schedule => '30 seconds',
    refresh_mode => 'DIFFERENTIAL'
);

-- Hot products: top-10 by order count in last 24 h
SELECT pgtrickle.create_stream_table(
    'public.hot_products',
    $$
        SELECT product, count(*) AS order_count
        FROM orders
        WHERE created_at > now() - interval '24 hours'
        GROUP BY product
        ORDER BY order_count DESC
        LIMIT 10
    $$,
    schedule => '1 minute',
    refresh_mode => 'FULL'
);

-- ── Seed data ──────────────────────────────────────────────────────────────

INSERT INTO products (sku, name, price)
VALUES
    ('SKU-001', 'Widget A', 9.99),
    ('SKU-002', 'Widget B', 19.99),
    ('SKU-003', 'Gadget X', 49.99),
    ('SKU-004', 'Gadget Y', 99.99)
ON CONFLICT (sku) DO NOTHING;

INSERT INTO orders (customer, product, amount)
SELECT
    'customer-' || (random() * 100)::int,
    (ARRAY['Widget A', 'Widget B', 'Gadget X', 'Gadget Y'])[1 + (random()*3)::int],
    (random() * 100 + 5)::numeric(10,2)
FROM generate_series(1, 1000);
