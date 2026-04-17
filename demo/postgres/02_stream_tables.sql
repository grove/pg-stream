-- =============================================================================
-- pg_trickle Real-time Fraud Detection Demo — Stream Table Definitions
--
-- All 7 stream tables created in topological order (sources before dependents).
-- =============================================================================

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 1 — Silver: direct aggregates from base tables
-- ─────────────────────────────────────────────────────────────────────────────

-- L1a: Per-user transaction velocity.
-- Tracks how often each user transacts, total spend, and how many distinct
-- merchants they've visited — core inputs for anomaly detection.
SELECT pgtrickle.create_stream_table(
    name     => 'user_velocity',
    query    => $$
        SELECT
            u.id               AS user_id,
            u.name             AS user_name,
            u.country,
            COUNT(t.id)                         AS txn_count,
            COALESCE(SUM(t.amount),   0)        AS total_spent,
            COALESCE(ROUND(AVG(t.amount), 2), 0) AS avg_txn_amount,
            COUNT(DISTINCT t.merchant_id)       AS unique_merchants
        FROM users u
        LEFT JOIN transactions t ON t.user_id = u.id
        GROUP BY u.id, u.name, u.country
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- L1b: Per-merchant transaction statistics.
-- Establishes a "normal" baseline for each merchant so that unusually large
-- transactions can be flagged relative to that merchant's history.
SELECT pgtrickle.create_stream_table(
    name     => 'merchant_stats',
    query    => $$
        SELECT
            m.id               AS merchant_id,
            m.name             AS merchant_name,
            m.category,
            COUNT(t.id)                          AS txn_count,
            COALESCE(SUM(t.amount),    0)        AS total_volume,
            COALESCE(ROUND(AVG(t.amount), 2), 0) AS avg_txn_amount,
            COUNT(DISTINCT t.user_id)            AS unique_users
        FROM merchants m
        LEFT JOIN transactions t ON t.merchant_id = m.id
        GROUP BY m.id, m.name, m.category
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- L1c: Transaction volume by merchant category.
-- Gives a fast aggregate view of which sectors are most active.
SELECT pgtrickle.create_stream_table(
    name     => 'category_volume',
    query    => $$
        SELECT
            m.category,
            COUNT(t.id)                          AS txn_count,
            COALESCE(SUM(t.amount),    0)        AS total_volume,
            COALESCE(ROUND(AVG(t.amount), 2), 0) AS avg_txn_amount,
            COUNT(DISTINCT t.user_id)            AS unique_users,
            COUNT(DISTINCT t.merchant_id)        AS active_merchants
        FROM merchants m
        LEFT JOIN transactions t ON t.merchant_id = m.id
        GROUP BY m.category
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 2 — Gold: derived metrics (stream tables reading other stream tables)
-- ─────────────────────────────────────────────────────────────────────────────

-- L2a: Per-transaction risk scoring.  ← THE DIAMOND NODE
--
-- This is the diamond dependency apex: it joins the base transactions table
-- with BOTH user_velocity (L1a) AND merchant_stats (L1b), which both derive
-- from transactions.  The scheduler refreshes L1 first, then this L2 ST.
--
-- Risk logic:
--   HIGH   — amount > 3× user's average  AND  user has made > 20 transactions
--   MEDIUM — amount > 2× user's average  OR   user has made > 10 transactions
--   LOW    — everything else
--
-- FULL refresh is used because each row depends on the *current* aggregate
-- state of user_velocity and merchant_stats; differential tracking across
-- three tables simultaneously is not yet implemented.
SELECT pgtrickle.create_stream_table(
    name     => 'risk_scores',
    query    => $$
        SELECT
            t.id                                            AS txn_id,
            t.user_id,
            COALESCE(uv.user_name,  u.name)                AS user_name,
            u.country                                      AS user_country,
            t.merchant_id,
            COALESCE(ms.merchant_name, m.name)             AS merchant_name,
            COALESCE(ms.category,      m.category)         AS merchant_category,
            t.amount,
            t.txn_at,
            COALESCE(uv.txn_count,        0)               AS user_txn_count,
            COALESCE(uv.avg_txn_amount,   t.amount)        AS user_avg_amount,
            COALESCE(ms.avg_txn_amount,   t.amount)        AS merchant_avg_amount,
            CASE
                WHEN COALESCE(uv.txn_count, 0) > 20
                     AND t.amount > 3 * COALESCE(uv.avg_txn_amount, t.amount)
                    THEN 'HIGH'
                WHEN COALESCE(uv.txn_count, 0) > 10
                     OR  t.amount > 2 * COALESCE(uv.avg_txn_amount, t.amount)
                    THEN 'MEDIUM'
                ELSE 'LOW'
            END                                            AS risk_level
        FROM transactions t
        JOIN users     u  ON u.id  = t.user_id
        JOIN merchants m  ON m.id  = t.merchant_id
        LEFT JOIN user_velocity  uv ON uv.user_id     = t.user_id
        LEFT JOIN merchant_stats ms ON ms.merchant_id = t.merchant_id
    $$,
    schedule     => 'calculated',
    refresh_mode => 'FULL'
);

-- L2b: Per-country risk aggregation (reads user_velocity L1a).
-- Shows geographic spread of transaction activity — useful for geo-based
-- fraud rules and compliance reporting.
SELECT pgtrickle.create_stream_table(
    name     => 'country_risk',
    query    => $$
        SELECT
            country,
            COUNT(*)                             AS user_count,
            SUM(txn_count)                       AS total_txns,
            SUM(total_spent)                     AS total_volume,
            ROUND(AVG(avg_txn_amount), 2)        AS avg_txn_amount,
            SUM(unique_merchants)                AS total_merchant_visits
        FROM user_velocity
        WHERE txn_count > 0
        GROUP BY country
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LAYER 3 — Platinum: executive roll-ups (reading L2 risk_scores)
-- ─────────────────────────────────────────────────────────────────────────────

-- L3a: Risk-level summary — counts and totals per risk bucket.
-- The primary KPI table: LOW / MEDIUM / HIGH transaction counts.
SELECT pgtrickle.create_stream_table(
    name     => 'alert_summary',
    query    => $$
        SELECT
            risk_level,
            COUNT(*)                      AS txn_count,
            SUM(amount)                   AS total_amount,
            ROUND(AVG(amount), 2)         AS avg_amount,
            MAX(txn_at)                   AS last_seen_at
        FROM risk_scores
        GROUP BY risk_level
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- L3b: Merchant risk league table — which merchants see the most flagged txns.
-- Operationally useful for blocking decisions and fraud team triage.
SELECT pgtrickle.create_stream_table(
    name     => 'top_risky_merchants',
    query    => $$
        SELECT
            merchant_name,
            merchant_category,
            COUNT(*)                                                        AS total_txns,
            SUM(CASE WHEN risk_level = 'HIGH'   THEN 1 ELSE 0 END)         AS high_risk_count,
            SUM(CASE WHEN risk_level = 'MEDIUM' THEN 1 ELSE 0 END)         AS medium_risk_count,
            SUM(CASE WHEN risk_level = 'LOW'    THEN 1 ELSE 0 END)         AS low_risk_count,
            ROUND(
                100.0 * SUM(CASE WHEN risk_level IN ('HIGH', 'MEDIUM') THEN 1 ELSE 0 END)
                / NULLIF(COUNT(*), 0),
                1
            )                                                               AS risk_rate_pct
        FROM risk_scores
        GROUP BY merchant_name, merchant_category
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
