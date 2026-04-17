# pg_trickle Real-time Demo

A self-contained Docker Compose demo showing real data flowing through stream
tables built on top of a continuous event feed.  Two showcase tables in each
scenario demonstrate differential refresh efficiency at sub-1.0 change ratios.

Three scenarios are available, selectable via the `DEMO_SCENARIO` environment
variable:

| Scenario | Default? | Description |
|----------|----------|-------------|
| `fraud` | — | Financial fraud detection pipeline — 9-node DAG over a transaction stream |
| `ecommerce` | ✅ | E-commerce analytics — 6-node DAG over a continuous order stream |
| `finance` | — | Financial risk analytics — 10-level deep DAG with only leaf schedules (CALCULATED throughout) |

## Quick start

```bash
cd demo

# E-commerce analytics (default)
docker compose up

# Fraud detection
DEMO_SCENARIO=fraud docker compose up

# Financial risk analytics (10-level DAG with deep calculated dependencies)
DEMO_SCENARIO=finance docker compose up
```

Then open **http://localhost:8080** in your browser.
The dashboard auto-refreshes every 2 seconds.

> **Switching scenarios** requires removing the old data volume:
> ```bash
> docker compose down -v
> DEMO_SCENARIO=fraud docker compose up
> ```
> Or use any of the three: `fraud`, `ecommerce`, `finance`.

---

## Building the Docker image from source

By default, the demo uses the pre-built `ghcr.io/grove/pg_trickle:latest` image from the GitHub Container Registry. This provides the fastest startup experience.

To test changes to the pg_trickle extension without waiting for an official release, you can build the Docker image from your current source code:

### Build the image

From the project root:

```bash
just build-demo
```

This builds a multi-stage Docker image named `pg_trickle:demo` that:
1. Compiles pg_trickle from your source code using Rust nightly
2. Installs it into PostgreSQL 18
3. Ready to use with the demo

The build takes 5–10 minutes depending on your system (Rust compilation is slow).

### Use the locally-built image

Once built, run the demo with your custom image:

```bash
cd demo
PG_TRICKLE_IMAGE=pg_trickle:demo docker compose down -v
PG_TRICKLE_IMAGE=pg_trickle:demo docker compose up
# or for ecommerce:
PG_TRICKLE_IMAGE=pg_trickle:demo DEMO_SCENARIO=ecommerce docker compose up
```

The `PG_TRICKLE_IMAGE` environment variable overrides the default pre-built image. The `-v` flag removes old volumes so the database reinitializes with the new extension.

### Available images

| Image | Usage | Notes |
|-------|-------|-------|
| `ghcr.io/grove/pg_trickle:latest` (default) | Pre-built from latest release | Fast startup (no build needed) |
| `pg_trickle:demo` | Build with `just build-demo` | Tests uncommitted source changes |

---

## Scenario: ecommerce (default)

The demo models a real-time **financial fraud detection system**.
Three services start together:

| Service     | Role                                                        |
|-------------|-------------------------------------------------------------|
| `postgres`  | PostgreSQL 18 + pg_trickle; initialises schema & stream tables |
| `generator` | Python script — inserts ~1 transaction/second continuously |
| `dashboard` | Flask web app — live dashboard at http://localhost:8080     |

Every ~45 seconds the generator triggers a **suspicious burst**: one user
fires 6–14 rapid, escalating-amount transactions at Crypto / Gambling
merchants, driving HIGH-risk scores and alerting the dashboard in real time.

Every ~30 generator cycles (roughly once per minute) the generator also
**rotates one merchant's risk tier** (STANDARD → ELEVATED → HIGH → STANDARD),
illustrating that `merchant_tier_stats` detects this change differentially.


---

## DAG topology (fraud scenario)

```
  Base tables            Layer 1 — Silver        Layer 2 — Gold          Layer 3 — Platinum
  ────────────           ──────────────────────   ─────────────────────   ──────────────────────

  ┌────────────┐         ┌──────────────────┐
  │   users    │────────►│  user_velocity   │─────────────────────────►┌──────────────────┐
  └────────────┘         │  (DIFFERENTIAL)  │                          │   country_risk   │
                         └──────┬───────────┘                          │  (DIFFERENTIAL)  │
                                │                                       └──────────────────┘
  ┌────────────┐                │  ┌──────────────────┐
  │transactions│────────────────┼─►│  merchant_stats  │
  │ (stream)   │                │  │  (DIFFERENTIAL)  │
  └────────────┘                │  └──────┬───────────┘
        │                       │         │
        │         ┌─────────────┼─────────┘  ← DIAMOND
        │         │             │
        │         ▼             ▼                                       ┌───────────────────────┐
        │    ┌────────────────────────┐                                 │    alert_summary      │
        │    │      risk_scores       │────────────────────────────────►│    (DIFFERENTIAL)     │
        │    │   (FULL, calculated)   │                                 └───────────────────────┘
        │    └────────────────────────┘
        │                                                               ┌───────────────────────┐
        │                                                               │  top_risky_merchants  │
        └──────────────────────────────────────────────────────────────►│    (DIFFERENTIAL)     │
                                                                        └──────────┬────────────┘
  ┌────────────┐         ┌──────────────────┐                                      │
  │ merchants  │────────►│ category_volume  │            ┌────────────────────────▼────────────┐
  └────────────┘         │  (DIFFERENTIAL)  │            │   top_10_risky_merchants            │
                         └──────────────────┘            │   DIFFERENTIAL 5s  ← SHOWCASE #2   │
                                                         │   change ratio ≈ 0.25 (LIMIT 10)   │
  ┌────────────────────┐  ┌──────────────────────┐       └─────────────────────────────────────┘
  │ merchant_risk_tier │─►│ merchant_tier_stats  │  ← SHOWCASE #1
  │ (slowly-changing)  │  │  DIFFERENTIAL 5s     │     change ratio ≈ 0.07
  └────────────────────┘  │                      │
  ┌────────────┐           │                      │
  │ merchants  │──────────►│                      │
  └────────────┘           └──────────────────────┘
```

### Stream tables

| Name                      | Layer    | Mode         | Schedule   | What it computes                                        |
|---------------------------|----------|--------------|------------|---------------------------------------------------------|
| `user_velocity`           | L1       | DIFFERENTIAL | 1 s        | Per-user: txn count, total spend, avg amount            |
| `merchant_stats`          | L1       | DIFFERENTIAL | 1 s        | Per-merchant: txn count, avg amount, unique users       |
| `category_volume`         | L1       | DIFFERENTIAL | 1 s        | Per-category: volume, avg amount, unique users          |
| `risk_scores`             | L2       | FULL         | calculated | Per-transaction: enriched with L1 + risk level          |
| `country_risk`            | L2       | DIFFERENTIAL | calculated | Per-country: roll-up from user_velocity                 |
| `alert_summary`           | L3       | DIFFERENTIAL | calculated | Per-risk-level: counts + totals from risk_scores        |
| `top_risky_merchants`     | L3       | DIFFERENTIAL | calculated | Per-merchant: risk counts from risk_scores              |
| `merchant_tier_stats`     | showcase | DIFFERENTIAL | 5 s        | Per-merchant: risk tier from slowly-changing lookup     |
| `top_10_risky_merchants`  | showcase | DIFFERENTIAL | 5 s        | Top-10 merchants by risk count (LIMIT 10 of L3)         |

### Why the diamond matters

`transactions` is the append-only source.  It feeds **two independent L1
stream tables** — `user_velocity` and `merchant_stats` — which are both then
consumed by `risk_scores` at L2.  This is a genuine **diamond dependency**:

```
transactions ──→ user_velocity  ──┐
                                  ├──→ risk_scores  (FULL, diamond convergence)
transactions ──→ merchant_stats ──┘
```

pg_trickle's DAG scheduler refreshes L1 nodes in topological order before
triggering the L2 refresh, ensuring `risk_scores` always sees up-to-date
aggregate context.

### Why `risk_scores` uses FULL refresh

`risk_scores` joins three sources: `transactions` (base), `user_velocity`
(L1 ST), and `merchant_stats` (L1 ST).  Differential maintenance across a
base table and two stream tables simultaneously is not yet implemented, so
FULL mode re-evaluates the entire join on each cycle.  The L3 tables
(`alert_summary`, `top_risky_merchants`) are DIFFERENTIAL because they only
read from the single ST upstream (`risk_scores`).

### Differential efficiency showcases

Two tables sit outside the main fraud pipeline to show the advisor in action
with sub-1.0 change ratios:

- **`merchant_tier_stats`** (Showcase #1) — joins `merchant_risk_tier` (15 rows,
  one updated per ~30 cycles) with `merchants` (static). Change ratio ≈ 0.07;
  the Refresh Mode Advisor confirms ✓ KEEP DIFFERENTIAL.

- **`top_10_risky_merchants`** (Showcase #2) — applies `LIMIT 10` to
  `top_risky_merchants`. Even though the upstream changes heavily, only rank
  boundary crossings produce output changes. Change ratio ≈ 0.25; advisor
  confirms ✓ KEEP DIFFERENTIAL.

---

## Playing with the demo

Connect directly to the database:

```bash
docker compose exec postgres psql -U demo -d fraud_demo
```

Useful queries:

```sql
-- See all stream table status
SELECT * FROM pgtrickle.pgt_status();

-- Inspect the DAG dependency graph (full tree)
SELECT tree_line FROM pgtrickle.dependency_tree()
ORDER BY tree_line;

-- Most recent HIGH-risk alerts
SELECT txn_id, user_name, merchant_name, amount, risk_level
FROM   risk_scores
WHERE  risk_level = 'HIGH'
ORDER  BY txn_id DESC LIMIT 10;

-- Trigger a manual refresh (normally the background worker does this)
SELECT pgtrickle.refresh_stream_table('risk_scores');

-- See refresh history
SELECT st.pgt_name, rh.action, rh.status, 
       (rh.end_time - rh.start_time) AS duration, rh.start_time
FROM pgtrickle.pgt_refresh_history rh
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = rh.pgt_id
ORDER  BY rh.start_time DESC LIMIT 20;
```

---

## Scenario: ecommerce

The e-commerce scenario models a real-time **online store analytics pipeline**
with orders streaming in continuously.

```bash
cd demo
DEMO_SCENARIO=ecommerce docker compose up
```

Every ~45 seconds the generator triggers a **flash sale** for one category:
a burst of 8–18 orders at a discount (70–90% of current price) floods in,
visible as a spike in the Revenue by Category panel.

Every ~30 generator cycles (roughly once per minute) the generator also
**reprices one product** in `product_catalog` (±20% of base price), driving a
targeted update in `catalog_price_impact` while leaving all other rows untouched.

### DAG topology (ecommerce)

```
  Base tables          Layer 1 — Silver           Layer 2 — Gold         Layer 3 — Platinum
  ────────────         ──────────────────────      ─────────────────────  ──────────────────────

  ┌────────────┐       ┌──────────────────┐
  │ customers  │──────►│ customer_stats   │──────────────────────────────►┌──────────────────┐
  └────────────┘       │  (DIFFERENTIAL)  │                               │ country_revenue  │
                       └──────────────────┘                               │  (DIFFERENTIAL)  │
                                │                                          └──────────────────┘
  ┌────────────┐                │
  │  orders    │────────────────┘
  │ (stream)   │────────────────────────────────►┌────────────────┐
  └────────────┘       ┌────────────────┐         │ product_sales  │
  ┌────────────┐       │category_revenue│         │ (DIFFERENTIAL) │
  │  products  │──────►│ (DIFFERENTIAL) │         └────────────────┘
  │ categories │──────►│                │
  └─────┬──────┘       └────────────────┘
        │
  ┌─────▼──────────────┐   ┌──────────────────────┐
  │  product_catalog   │──►│ catalog_price_impact │  ← DIFFERENTIAL SHOWCASE #1
  │  (slowly-changing) │   │   (DIFFERENTIAL 5s)  │    change ratio ~0.07
  └────────────────────┘   └──────────────────────┘

  ┌──────────────────┐   ┌──────────────────┐
  │  customer_stats  │──►│  top_10_customers│  ← DIFFERENTIAL SHOWCASE #2
  │  (DIFFERENTIAL)  │   │  (DIFFERENTIAL)  │    change ratio ~0.1–0.2
  └──────────────────┘   │  LIMIT 10        │
                         └──────────────────┘
```

### Stream tables (ecommerce)

| Name                   | Layer    | Mode         | Schedule   | What it computes                                        |
|------------------------|----------|--------------|------------|---------------------------------------------------------|
| `product_sales`        | L1       | DIFFERENTIAL | 1 s        | Per-product: units sold, revenue, avg price             |
| `customer_stats`       | L1       | DIFFERENTIAL | 1 s        | Per-customer: order count, total spent, avg order value |
| `category_revenue`     | L1       | DIFFERENTIAL | 1 s        | Per-category: orders, units, revenue, avg price         |
| `country_revenue`      | L2       | DIFFERENTIAL | calculated | Per-country: roll-up from customer_stats                |
| `catalog_price_impact` | showcase | DIFFERENTIAL | 5 s        | Per-product: current vs base price (slowly-changing)    |
| `top_10_customers`     | showcase | DIFFERENTIAL | calculated | Top 10 customers by total spend (LIMIT 10)              |

### Playing with the ecommerce demo

```bash
docker compose exec postgres psql -U demo -d ecommerce_demo
```

```sql
-- See category revenue live
SELECT * FROM category_revenue ORDER BY revenue DESC;

-- Top 10 customers leaderboard
SELECT * FROM top_10_customers;

-- Price changes in the catalog
SELECT product_name, base_price, current_price, pct_change
FROM   catalog_price_impact
ORDER  BY ABS(pct_change) DESC;

-- Refresh efficiency comparison
SELECT pgt_name, avg_diff_ms, diff_speedup, avg_change_ratio
FROM   pgtrickle.refresh_efficiency()
ORDER  BY pgt_name;
```

---

## Scenario: finance

The finance scenario models a real-time **financial risk analytics pipeline** with
a **10-level deep DAG** showing how DIFFERENTIAL mode excels in deeply-nested
stream table cascades. Only the two leaf tables have fixed schedules (2s and 1s);
all downstream layers use `schedule => 'calculated'` to propagate changes
automatically through the full computation graph.

```bash
cd demo
DEMO_SCENARIO=finance docker compose up
```

Every generator cycle, one of 30 instruments updates its bid/ask/mid prices with
realistic market movement (Gaussian walk + mean reversion). Trades flow in
continuously at ~1/second, with occasional algorithmic bursts (every ~60 seconds).

### The 10-Level Cascade

```
L1 — price_snapshot (2s)  +  net_positions (1s)  ← Two leaf tables with fixed schedules
  ↓
L2 — position_values (CALCULATED)  ← All downstream layers cascade automatically
  ↓
L3 — account_pnl (CALCULATED)
  ↓
L4 — portfolio_pnl (CALCULATED)
  ↓
L5 — sector_exposure (CALCULATED)
  ↓
L6 — var_contributions (CALCULATED, parametric VaR per position)
  ↓
L7 — account_var (CALCULATED, aggregated VaR)
  ↓
L8 — portfolio_var (CALCULATED, Basel scenario VaR)
  ↓
L9 — regulatory_capital (CALCULATED, capital requirement)
  ↓
L10 — breach_dashboard (CALCULATED, LIMIT 10, top capital-constrained portfolios)
```

### Stream Tables (finance)

| Layer | Name | Mode | Schedule | Cardinality |
|-------|------|------|----------|-------------|
| L1 | `price_snapshot` | DIFFERENTIAL | 2s | 30 instruments |
| L1 | `net_positions` | DIFFERENTIAL | 1s | ≤ 1,500 account-instrument pairs |
| L2 | `position_values` | DIFFERENTIAL | calculated | ≤ 1,500 positions (marked-to-market) |
| L3 | `account_pnl` | DIFFERENTIAL | calculated | ≤ 50 accounts |
| L4 | `portfolio_pnl` | DIFFERENTIAL | calculated | 5 portfolios |
| L5 | `sector_exposure` | DIFFERENTIAL | calculated | 8 sectors |
| L6 | `var_contributions` | DIFFERENTIAL | calculated | ≤ 1,500 positions (VaR per position) |
| L7 | `account_var` | DIFFERENTIAL | calculated | ≤ 50 accounts |
| L8 | `portfolio_var` | DIFFERENTIAL | calculated | 5 portfolios |
| L9 | `regulatory_capital` | DIFFERENTIAL | calculated | 5 portfolios |
| L10 | `breach_dashboard` | DIFFERENTIAL | calculated | ≤ 10 portfolios (ranked by capital ratio) |

### Differential Efficiency: Cardinality Compression

This DAG beautifully demonstrates DIFFERENTIAL's strength in deep pipelines:

- **L1 → L2**: 30 instruments feed 1,500 positions. Each price tick changes ~30 positions (ratio ≈ 0.02).
- **L2 → L5**: Position changes aggregate and compress. By L5 (sector exposure), only 8 rows, typically 1 changes per tick.
- **L5 → L10**: Further aggregation to 5 portfolios at L8, then filtered to top 10 by rank at L10. L10 changes are rare (typically 0–1 row per tick).

All layers use DIFFERENTIAL because each depends on only one upstream source and
applies straightforward aggregation or filtering — no complex multi-source joins.

### Playing with the finance demo

```bash
docker compose exec postgres psql -U demo -d finance_demo
```

```sql
-- Current market prices (L1)
SELECT symbol, sector, bid, ask, mid FROM price_snapshot ORDER BY symbol;

-- Account net positions (L1)
SELECT account_id, instrument_symbol, net_quantity, position_value
FROM   net_positions WHERE net_quantity != 0;

-- Portfolio P&L and VaR (L4 and L8)
SELECT portfolio_name, pnl, portfolio_var_95pct
FROM   portfolio_var ORDER BY portfolio_name;

-- Top 10 capital-constrained portfolios (L10)
SELECT rank, portfolio_name, capital_required, capital_limit, utilization_ratio
FROM   breach_dashboard ORDER BY rank;

-- Observe change ratios: L1 high, L10 very low
SELECT pgt_name, avg_change_ratio, diff_speedup
FROM   pgtrickle.refresh_efficiency()
WHERE  pgt_id <= 10
ORDER  BY pgt_id;

-- Watch real-time refresh cascade (one-liner to see last 5 seconds)
SELECT st.pgt_name, rh.action, (rh.end_time - rh.start_time)::int8 AS ms
FROM pgtrickle.pgt_refresh_history rh
JOIN pgtrickle.pgt_stream_tables st ON st.pgt_id = rh.pgt_id
WHERE rh.start_time > now() - interval '5 seconds'
ORDER BY rh.start_time DESC;
```

---

## Stopping the demo

```bash
docker compose down -v   # also removes the pgdata volume
```
