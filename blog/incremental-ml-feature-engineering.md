[← Back to Blog Index](README.md)

# Incremental ML Feature Engineering in PostgreSQL

## Replace your nightly feature store batch job with continuously fresh features maintained as stream tables

---

Machine learning models are only as good as their features. And features are only as good as their freshness. A fraud detection model trained on "average transaction amount in the last 7 days" is useless if that feature was computed yesterday and the fraudster has been active for the last 6 hours with abnormally large transactions.

The standard ML feature engineering pipeline looks like this: a scheduled job (Airflow, dbt, cron) runs every hour or every day, reads raw data, computes derived features, and writes them to a feature store. The model reads features from the store at inference time. The lag between raw data and computed features ranges from minutes to hours, depending on your pipeline's schedule and execution time.

pg_trickle collapses this pipeline into a single layer. Features are defined as SQL queries over your operational data, maintained as stream tables that update incrementally as the underlying data changes. The feature store is just a set of materialized views that are always fresh. No pipeline orchestration, no batch jobs, no staleness window.

---

## Features as Stream Tables

Consider a fraud detection model that uses these features per customer:

- Average transaction amount (last 7 days)
- Transaction count (last 24 hours)
- Number of distinct merchants (last 7 days)
- Maximum single transaction (last 30 days)
- Standard deviation of transaction amounts (last 7 days)
- Ratio of current transaction to historical average

Each of these is a SQL aggregate over the transactions table with a time window. Traditionally, you'd compute them in a batch job:

```sql
-- Traditional batch feature computation (runs hourly via Airflow)
INSERT INTO customer_features
SELECT
    customer_id,
    AVG(amount) FILTER (WHERE created_at > now() - interval '7 days') AS avg_amount_7d,
    COUNT(*) FILTER (WHERE created_at > now() - interval '24 hours') AS txn_count_24h,
    COUNT(DISTINCT merchant_id) FILTER (WHERE created_at > now() - interval '7 days') AS distinct_merchants_7d,
    MAX(amount) FILTER (WHERE created_at > now() - interval '30 days') AS max_amount_30d,
    STDDEV(amount) FILTER (WHERE created_at > now() - interval '7 days') AS stddev_amount_7d
FROM transactions
GROUP BY customer_id;
```

This scans the entire transactions table every hour. For 100 million transactions across 5 million customers, that's a multi-minute full table scan every hour. And between runs, the features are stale.

As stream tables:

```sql
SELECT pgtrickle.create_stream_table(
    'customer_features_7d',
    $$
    SELECT
        customer_id,
        AVG(amount) AS avg_amount,
        COUNT(*) AS txn_count,
        COUNT(DISTINCT merchant_id) AS distinct_merchants,
        STDDEV(amount) AS stddev_amount,
        MAX(amount) AS max_amount
    FROM transactions
    WHERE created_at > now() - interval '7 days'
    GROUP BY customer_id
    $$
);
```

Every new transaction incrementally updates the affected customer's features. The cost per transaction is constant — one group update — regardless of how many historical transactions exist. Feature freshness drops from hours to seconds.

---

## Rolling Window Features

Time-windowed aggregates are the bread and butter of ML feature engineering. "Sum of deposits in the last 30 days," "count of logins in the last hour," "moving average of price over 20 periods."

pg_trickle handles these by tracking both additions (new rows entering the window) and removals (old rows falling out of the window). When a new transaction arrives, it's added to the aggregate. When the window advances past an old transaction, it's subtracted.

```sql
-- Transaction velocity: count and sum in sliding 1-hour window
SELECT pgtrickle.create_stream_table(
    'customer_velocity_1h',
    $$
    SELECT
        customer_id,
        COUNT(*) AS txn_count_1h,
        SUM(amount) AS total_amount_1h,
        MAX(amount) AS max_amount_1h
    FROM transactions
    WHERE created_at > now() - interval '1 hour'
    GROUP BY customer_id
    $$
);
```

For fraud detection, velocity features are critical. A customer who normally makes 2 transactions per hour suddenly making 15 is a strong signal. With batch computation, you might not detect this until the next hourly run — by which time the fraudster has already completed all 15 transactions and disappeared. With incremental maintenance, the velocity feature updates after each transaction, and the model can score in real time.

---

## Lag Features and Sequential Patterns

ML models for time-series prediction often use lag features: "what was the value N steps ago?" In SQL:

```sql
SELECT pgtrickle.create_stream_table(
    'customer_transaction_lags',
    $$
    SELECT
        customer_id,
        amount AS latest_amount,
        LAG(amount, 1) OVER w AS prev_amount_1,
        LAG(amount, 2) OVER w AS prev_amount_2,
        LAG(amount, 3) OVER w AS prev_amount_3,
        amount - LAG(amount, 1) OVER w AS amount_delta,
        created_at - LAG(created_at, 1) OVER w AS time_since_last
    FROM transactions
    WINDOW w AS (PARTITION BY customer_id ORDER BY created_at)
    $$
);
```

Window functions with LAG and LEAD are maintained incrementally by pg_trickle. When a new transaction arrives for a customer, the lag values shift: the previous "latest" becomes `prev_amount_1`, the previous `prev_amount_1` becomes `prev_amount_2`, and so on. Only the affected customer's row is updated.

---

## Cross-Entity Features

Some of the most powerful features relate an entity to its peers. "How does this customer's spending compare to the average for their cohort?" "Is this merchant's chargeback rate above the industry median?"

```sql
-- Merchant risk features: how does each merchant compare to peers?
SELECT pgtrickle.create_stream_table(
    'merchant_risk_features',
    $$
    SELECT
        m.merchant_id,
        m.category,
        COUNT(t.id) AS total_transactions,
        SUM(CASE WHEN t.is_chargeback THEN 1 ELSE 0 END) AS chargeback_count,
        SUM(CASE WHEN t.is_chargeback THEN 1 ELSE 0 END)::float
            / NULLIF(COUNT(t.id), 0) AS chargeback_rate,
        AVG(t.amount) AS avg_transaction_amount
    FROM merchants m
    LEFT JOIN transactions t ON t.merchant_id = m.merchant_id
        AND t.created_at > now() - interval '30 days'
    GROUP BY m.merchant_id, m.category
    $$
);
```

When a chargeback is recorded, only the affected merchant's features are recomputed. When you need to compare a merchant to its category average, that's another stream table on top:

```sql
SELECT pgtrickle.create_stream_table(
    'merchant_category_benchmarks',
    $$
    SELECT
        category,
        AVG(chargeback_rate) AS category_avg_chargeback_rate,
        AVG(avg_transaction_amount) AS category_avg_txn_amount,
        COUNT(*) AS merchants_in_category
    FROM merchant_risk_features
    GROUP BY category
    $$
);
```

The cascade maintains both the per-merchant features and the category benchmarks incrementally. A single chargeback event propagates through: transaction → merchant features → category benchmark. Total cost: a few milliseconds.

---

## Feature Freshness vs. Feature Stores

Traditional feature stores (Feast, Tecton, Hopsworks) optimize for serving pre-computed features at low latency. They're excellent at that specific job. But they introduce a batch computation → store → serve pipeline that adds latency and operational complexity.

| Aspect | Batch feature store | pg_trickle stream tables |
|--------|-------------------|--------------------------|
| Feature freshness | Minutes to hours (batch interval) | Seconds (refresh interval) |
| Infrastructure | Airflow + Spark/dbt + Redis/DynamoDB | PostgreSQL (already have it) |
| Feature definition | Python/SQL in DAG configs | SQL (stream table definition) |
| Backfill new features | Full historical recompute | Initial materialization + incremental |
| Point-in-time correctness | Complex (time-travel logic) | Automatic (SQL windowing) |

The key insight is that if your features can be expressed as SQL aggregates (and most can), then they can be maintained incrementally inside the database. You don't need a separate compute layer, a separate storage layer, and a separate serving layer. The database is all three.

---

## Real-Time Scoring Pipeline

The ultimate payoff of incremental features is real-time scoring. Instead of looking up pre-computed (stale) features at inference time, the model reads features that are current as of the last transaction:

```python
# At inference time: features are always fresh
def score_transaction(customer_id: str, amount: float) -> float:
    # Features are maintained incrementally by pg_trickle
    features = db.execute("""
        SELECT avg_amount, txn_count, distinct_merchants, stddev_amount
        FROM customer_features_7d
        WHERE customer_id = %s
    """, [customer_id]).fetchone()

    # Score with fresh features
    return model.predict([
        amount,
        features.avg_amount,
        features.txn_count,
        features.distinct_merchants,
        features.stddev_amount,
        amount / features.avg_amount if features.avg_amount > 0 else 0,
    ])
```

The features in `customer_features_7d` reflect all transactions up to the most recent refresh (typically seconds ago). No feature store lookup, no cache invalidation, no staleness. Just a table read.

---

## Getting Started

```sql
-- Your transaction table (already exists in most systems)
CREATE TABLE transactions (
    id           bigserial PRIMARY KEY,
    customer_id  text NOT NULL,
    merchant_id  text NOT NULL,
    amount       numeric(12,2),
    created_at   timestamptz DEFAULT now(),
    is_chargeback boolean DEFAULT false
);

-- Define features as a stream table
SELECT pgtrickle.create_stream_table(
    'fraud_features',
    $$
    SELECT
        customer_id,
        COUNT(*) AS total_txns,
        AVG(amount) AS avg_amount,
        STDDEV(amount) AS amount_stddev,
        MAX(amount) AS max_amount,
        COUNT(DISTINCT merchant_id) AS unique_merchants
    FROM transactions
    WHERE created_at > now() - interval '7 days'
    GROUP BY customer_id
    $$
);

-- Features update automatically as transactions flow in
INSERT INTO transactions (customer_id, merchant_id, amount)
VALUES ('cust_123', 'merch_456', 299.99);

SELECT pgtrickle.refresh_stream_table('fraud_features');
-- cust_123's features are now current
```

Your feature store is a stream table. Your feature pipeline is `CREATE STREAM TABLE`. Your feature freshness is the refresh interval. It's that simple.

---

*Stop waiting for batch jobs to compute stale features. Let the database maintain them incrementally, and let your models score with fresh data.*
