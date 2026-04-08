# Playground

The quickest way to explore pg_trickle is the **playground** — a pre-configured
Docker environment with sample data and stream tables ready to query. No
installation, no configuration. One command and you're running.

## Quick Start

```bash
git clone https://github.com/grove/pg-trickle.git
cd pg-trickle/playground
docker compose up -d
```

Then connect:

```bash
psql postgresql://postgres:playground@localhost:5432/playground
```

> **PostgreSQL 18+ note:** The Docker image stores data in a versioned
> subdirectory (`/var/lib/postgresql/18/main`). The compose file mounts
> `/var/lib/postgresql` (not `.../data`) — this is intentional.

---

## What's Pre-Loaded

The seed script creates three base tables and five stream tables that cover
the most common pg_trickle patterns.

### Base Tables

| Table | Description |
|-------|-------------|
| `products` | Product catalog with categories and prices |
| `orders` | Order line items with quantities and timestamps |
| `customers` | Customer profiles with regions |

### Stream Tables

| Stream Table | Query | Pattern demonstrated |
|---|---|---|
| `sales_by_region` | `SUM(total)` grouped by region | Basic aggregate, DIFFERENTIAL mode |
| `top_products` | `SUM(quantity)` ranked by category | Window function (`RANK()`) |
| `customer_lifetime_value` | Revenue + order count per customer | Multi-table join + aggregates |
| `daily_revenue` | Revenue per day | Time-series aggregation |
| `active_products` | Products with orders | `EXISTS` subquery |

---

## Exercises

### 1. Watch an INSERT propagate

```sql
-- Current state
SELECT * FROM sales_by_region ORDER BY region;

-- Insert a new order
INSERT INTO orders (customer_id, product_id, quantity, order_date)
VALUES (1, 1, 10, CURRENT_DATE);

-- After ~1 s the stream table refreshes
SELECT * FROM sales_by_region ORDER BY region;
```

### 2. Inspect pg_trickle internals

```sql
-- Overall health
SELECT * FROM pgtrickle.health_check();

-- Status of all stream tables
SELECT name, status, refresh_mode, staleness
FROM pgtrickle.pgt_status()
ORDER BY name;

-- Recent refresh activity
SELECT start_time, stream_table, action, status, duration_ms
FROM pgtrickle.refresh_timeline(10);

-- Delta SQL for a stream table
SELECT pgtrickle.explain_st('sales_by_region');

-- Change buffer sizes
SELECT * FROM pgtrickle.change_buffer_sizes();
```

### 3. Update and Delete

```sql
-- Update a product price
UPDATE products SET price = 99.99 WHERE name = 'Widget';

-- customer_lifetime_value re-calculates
SELECT * FROM customer_lifetime_value ORDER BY total_revenue DESC LIMIT 5;

-- Delete a customer's orders
DELETE FROM orders WHERE customer_id = 3;

-- Stream tables reflect the removal
SELECT * FROM sales_by_region ORDER BY region;
```

### 4. Create your own stream table

```sql
SELECT pgtrickle.create_stream_table(
    name     => 'my_experiment',
    query    => $$
        SELECT p.category,
               COUNT(DISTINCT o.customer_id) AS unique_buyers,
               SUM(o.quantity)               AS total_units
        FROM orders o
        JOIN products p ON p.id = o.product_id
        GROUP BY p.category
        HAVING SUM(o.quantity) > 5
    $$,
    schedule => '2s'
);

SELECT * FROM my_experiment;
```

---

## Tear Down

```bash
docker compose down -v
```

The `-v` flag removes the data volume. Omit it if you want to keep your changes.

---

## Next Steps

- [Getting Started Guide](GETTING_STARTED.md) — full tutorial with an org-chart example
- [SQL Reference](SQL_REFERENCE.md) — all functions and parameters
- [Best-Practice Patterns](PATTERNS.md) — production-ready patterns
