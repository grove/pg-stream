-- =============================================================================
-- pg_trickle Real-time E-commerce Analytics Demo — Stream Table Definitions
--
-- All 6 stream tables created in topological order (sources before dependents).
-- =============================================================================

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 1 — Silver: direct aggregates from base tables
-- ─────────────────────────────────────────────────────────────────────────────

-- L1a: Per-product sales statistics.
-- Tracks units sold, total revenue, and average selling price per product.
-- Core input for category and price-impact analysis.
SELECT pgtrickle.create_stream_table(
    name     => 'product_sales',
    query    => $$
        SELECT
            p.id                                            AS product_id,
            p.name                                         AS product_name,
            cat.name                                       AS category_name,
            COUNT(o.id)                                    AS order_count,
            COALESCE(SUM(o.quantity), 0)                   AS units_sold,
            COALESCE(SUM(o.quantity * o.unit_price), 0)    AS revenue,
            COALESCE(ROUND(AVG(o.unit_price), 2), 0)       AS avg_selling_price
        FROM products p
        JOIN categories cat ON cat.id = p.category_id
        LEFT JOIN orders o ON o.product_id = p.id
        GROUP BY p.id, p.name, cat.id, cat.name
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- L1b: Per-customer purchase statistics.
-- Tracks order count, total spend, and average order value per customer.
-- Core input for country-level aggregation and the top-customers leaderboard.
SELECT pgtrickle.create_stream_table(
    name     => 'customer_stats',
    query    => $$
        SELECT
            c.id                                               AS customer_id,
            c.name                                            AS customer_name,
            c.country,
            COUNT(o.id)                                       AS order_count,
            COALESCE(SUM(o.quantity * o.unit_price), 0)       AS total_spent,
            COALESCE(ROUND(AVG(o.quantity * o.unit_price), 2), 0) AS avg_order_value
        FROM customers c
        LEFT JOIN orders o ON o.customer_id = c.id
        GROUP BY c.id, c.name, c.country
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- L1c: Revenue and order volume by category.
-- Gives a fast aggregate view of which categories are most active.
SELECT pgtrickle.create_stream_table(
    name     => 'category_revenue',
    query    => $$
        SELECT
            cat.id                                          AS category_id,
            cat.name                                        AS category_name,
            COUNT(o.id)                                     AS order_count,
            COALESCE(SUM(o.quantity), 0)                    AS units_sold,
            COALESCE(SUM(o.quantity * o.unit_price), 0)     AS revenue,
            COALESCE(ROUND(AVG(o.unit_price), 2), 0)        AS avg_price,
            COUNT(DISTINCT o.customer_id)                   AS unique_customers
        FROM categories cat
        LEFT JOIN products p   ON p.category_id = cat.id
        LEFT JOIN orders   o   ON o.product_id  = p.id
        GROUP BY cat.id, cat.name
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 2 — Gold: derived metrics (stream tables reading other stream tables)
-- ─────────────────────────────────────────────────────────────────────────────

-- L2a: Per-country revenue summary (reads customer_stats L1b).
-- Shows geographic spread of purchasing activity.
SELECT pgtrickle.create_stream_table(
    name     => 'country_revenue',
    query    => $$
        SELECT
            country,
            COUNT(*)                             AS customer_count,
            SUM(order_count)                     AS total_orders,
            SUM(total_spent)                     AS total_revenue,
            ROUND(AVG(avg_order_value), 2)       AS avg_order_value
        FROM customer_stats
        WHERE order_count > 0
        GROUP BY country
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- DIFFERENTIAL EFFICIENCY SHOWCASE #1
-- ─────────────────────────────────────────────────────────────────────────────
-- catalog_price_impact demonstrates a low change ratio by depending ONLY on
-- product_catalog — a slowly-changing lookup that the generator updates for
-- one product every ~30 cycles (roughly once per minute).
--
-- Per-cycle change profile:
--   • product_catalog: 1 row updated every ~30 cycles → 1 of 15 rows changes.
--   • products: static lookup — never updated, 0 changes ever.
--
-- Result: change ratio ≈ 1/15 ≈ 0.07 — the Refresh Mode Advisor will show
-- KEEP DIFFERENTIAL here.
SELECT pgtrickle.create_stream_table(
    name     => 'catalog_price_impact',
    query    => $$
        SELECT
            pc.product_id,
            p.name                                         AS product_name,
            cat.name                                       AS category_name,
            p.base_price,
            pc.current_price,
            ROUND(pc.current_price - p.base_price, 2)     AS price_delta,
            ROUND(
                100.0 * (pc.current_price - p.base_price)
                / NULLIF(p.base_price, 0), 1
            )                                              AS pct_change,
            pc.updated_at                                  AS price_last_updated
        FROM product_catalog pc
        JOIN products    p   ON p.id  = pc.product_id
        JOIN categories  cat ON cat.id = p.category_id
    $$,
    schedule     => '5s',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 3 — Platinum: executive roll-ups
-- ─────────────────────────────────────────────────────────────────────────────

-- ─────────────────────────────────────────────────────────────────────────────
-- DIFFERENTIAL EFFICIENCY SHOWCASE #2
-- ─────────────────────────────────────────────────────────────────────────────
-- top_10_customers demonstrates low change ratio via fixed cardinality.
-- Only the top 10 customers are output; only rank shifts matter.
--
-- Per-cycle change profile:
--   • customer_stats can change whenever a new order arrives (every ~1s).
--   • top_10_customers: LIMIT 10 → typically only 1–2 customers swap ranks,
--     not all 30 customer rows.
--
-- Result: change ratio ≈ 0.1–0.2 (fixed 10-row output; only rank shifts).
-- The Refresh Mode Advisor will show KEEP DIFFERENTIAL here.
SELECT pgtrickle.create_stream_table(
    name     => 'top_10_customers',
    query    => $$
        SELECT
            ROW_NUMBER() OVER (ORDER BY total_spent DESC, customer_name) AS rank,
            customer_name,
            country,
            order_count,
            total_spent,
            avg_order_value
        FROM customer_stats
        WHERE order_count > 0
        ORDER BY total_spent DESC, customer_name
        LIMIT 10
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
