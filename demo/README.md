# pg_trickle Real-time Demo — Fraud Detection Pipeline

A self-contained Docker Compose demo that shows real data flowing through a
**9-node DAG** of stream tables built on top of a continuous transaction feed.
Two showcase tables demonstrate differential refresh efficiency at sub-1.0
change ratios.

## Quick start

```bash
cd demo
docker compose up
```

Then open **http://localhost:8080** in your browser.
The dashboard auto-refreshes every 2 seconds.

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
```

The `PG_TRICKLE_IMAGE` environment variable overrides the default pre-built image. The `-v` flag removes old volumes so the database reinitializes with the new extension.

### Available images

| Image | Usage | Notes |
|-------|-------|-------|
| `ghcr.io/grove/pg_trickle:latest` (default) | Pre-built from latest release | Fast startup (no build needed) |
| `pg_trickle:demo` | Build with `just build-demo` | Tests uncommitted source changes |

---

## What you'll see

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

## DAG topology

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

## Stopping the demo

```bash
docker compose down -v   # also removes the pgdata volume
```
