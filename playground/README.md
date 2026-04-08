# pg_trickle Playground

A one-command Docker environment for exploring pg_trickle with pre-loaded
sample data. No installation or configuration required.

## Quick Start

```bash
docker compose up -d
```

This starts PostgreSQL 18 with pg_trickle pre-installed, creates sample tables,
and sets up stream tables that demonstrate key features.

Connect:

```bash
psql postgresql://postgres:playground@localhost:5432/playground
```

## What's Inside

The seed script (`seed.sql`) creates:

### Base Tables

| Table | Description |
|-------|-------------|
| `products` | Product catalog with categories and prices |
| `orders` | Order line items with quantities and timestamps |
| `customers` | Customer profiles with regions |

### Stream Tables

| Stream Table | Query | Demonstrates |
|--------------|-------|-------------|
| `sales_by_region` | `SUM(total)` grouped by region | Basic aggregate with DIFFERENTIAL mode |
| `top_products` | `SUM(quantity)` ranked by category | Window function (`RANK()`) |
| `customer_lifetime_value` | Revenue + order count per customer | Multi-table join + aggregates |
| `daily_revenue` | Revenue per day | Time-series aggregation |
| `active_products` | Products with orders | EXISTS subquery |

## Try It

After starting the playground, try these exercises:

### 1. Watch an INSERT propagate

```sql
-- Check current state
SELECT * FROM sales_by_region ORDER BY region;

-- Insert a new order
INSERT INTO orders (customer_id, product_id, quantity, order_date)
VALUES (1, 1, 10, CURRENT_DATE);

-- Wait ~1 second for the refresh, then check again
SELECT * FROM sales_by_region ORDER BY region;
```

### 2. Inspect how pg_trickle works

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

-- See the delta SQL for a stream table
SELECT pgtrickle.explain_st('sales_by_region');

-- CDC pipeline health
SELECT * FROM pgtrickle.change_buffer_sizes();
```

### 3. Try an UPDATE and DELETE

```sql
-- Update a product price
UPDATE products SET price = 99.99 WHERE name = 'Widget';

-- After refresh, customer_lifetime_value re-calculates
SELECT * FROM customer_lifetime_value ORDER BY total_revenue DESC LIMIT 5;

-- Delete a customer's orders
DELETE FROM orders WHERE customer_id = 3;

-- Stream tables update to reflect the removal
SELECT * FROM sales_by_region ORDER BY region;
```

### 4. Create your own stream table

```sql
SELECT pgtrickle.create_stream_table(
    name     => 'my_experiment',
    query    => $$
        SELECT p.category,
               COUNT(DISTINCT o.customer_id) AS unique_buyers,
               SUM(o.quantity) AS total_units
        FROM orders o
        JOIN products p ON p.id = o.product_id
        GROUP BY p.category
        HAVING SUM(o.quantity) > 5
    $$,
    schedule => '2s'
);

SELECT * FROM my_experiment;
```

## Tear Down

```bash
docker compose down -v
```

## Terminal UI

For a live monitoring dashboard, use the `pgtrickle` TUI:

```bash
# Install (once, requires Rust toolchain)
cargo install --path ../pgtrickle-tui

# Make sure ~/.cargo/bin is on your PATH (add to ~/.bashrc or ~/.zshrc if needed)
export PATH="$HOME/.cargo/bin:$PATH"

# Launch against the playground
pgtrickle --url postgresql://postgres:playground@localhost:5432/playground
```

This opens a full-screen interactive dashboard that auto-refreshes every 2 seconds.
Switch views with the number keys or letters:

| Key | View |
|-----|------|
| `1` | Dashboard — all stream tables with status and staleness |
| `2` | Detail — deep dive into the selected table |
| `3` | Dependencies — ASCII dependency tree |
| `4` | Refresh Log — timeline of recent refreshes |
| `5` | Diagnostics — recommended refresh mode per table |
| `6` | CDC Health — change buffer sizes |
| `8` | Health Checks — extension health summary |
| `d` | Delta Inspector — auto-generated delta SQL |

Useful keys: `r` refresh selected, `R` refresh all, `/` filter, `?` help, `q` quit.

See the full [TUI User Guide](../docs/TUI.md) for all views, keyboard shortcuts, and CLI subcommands.

## Next Steps

- [Getting Started Guide](../docs/GETTING_STARTED.md) — full tutorial with org-chart example
- [SQL Reference](../docs/SQL_REFERENCE.md) — all functions and configuration
- [Patterns](../docs/PATTERNS.md) — best-practice patterns for production use
- [TUI User Guide](../docs/TUI.md) — terminal UI reference
