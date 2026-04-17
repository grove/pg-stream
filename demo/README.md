# pg_trickle Real-time Demo — Fraud Detection Pipeline

A self-contained Docker Compose demo that shows real data flowing through a
**7-node, 3-layer DAG** of stream tables built on top of a continuous
transaction feed.

## Quick start

```bash
cd demo
docker compose up --build
```

Then open **http://localhost:8080** in your browser.
The dashboard auto-refreshes every 2 seconds.

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
                                                                        └───────────────────────┘
  ┌────────────┐         ┌──────────────────┐
  │ merchants  │────────►│ category_volume  │
  └────────────┘         │  (DIFFERENTIAL)  │
                         └──────────────────┘
```

### Stream tables

| Name                  | Layer | Mode         | Schedule    | What it computes                                  |
|-----------------------|-------|--------------|-------------|---------------------------------------------------|
| `user_velocity`       | L1    | DIFFERENTIAL | 1 s         | Per-user: txn count, total spend, avg amount      |
| `merchant_stats`      | L1    | DIFFERENTIAL | 1 s         | Per-merchant: txn count, avg amount, unique users |
| `category_volume`     | L1    | DIFFERENTIAL | 1 s         | Per-category: volume, avg amount, unique users    |
| `risk_scores`         | L2    | FULL         | calculated  | Per-transaction: enriched with L1 + risk level   |
| `country_risk`        | L2    | DIFFERENTIAL | calculated  | Per-country: roll-up from user_velocity           |
| `alert_summary`       | L3    | DIFFERENTIAL | calculated  | Per-risk-level: counts + totals from risk_scores  |
| `top_risky_merchants` | L3    | DIFFERENTIAL | calculated  | Per-merchant: risk counts from risk_scores        |

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

-- Inspect the DAG dependency graph
SELECT * FROM pgtrickle.dependency_tree('risk_scores');

-- Most recent HIGH-risk alerts
SELECT txn_id, user_name, merchant_name, amount, risk_level
FROM   risk_scores
WHERE  risk_level = 'HIGH'
ORDER  BY txn_id DESC LIMIT 10;

-- Trigger a manual refresh (normally the background worker does this)
SELECT pgtrickle.refresh_stream_table('risk_scores');

-- See refresh history
SELECT * FROM pgtrickle.pgt_refresh_history
ORDER  BY refreshed_at DESC LIMIT 20;
```

---

## Stopping the demo

```bash
docker compose down -v   # also removes the pgdata volume
```
