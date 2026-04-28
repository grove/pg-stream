[← Back to Blog Index](README.md)

# Funnel Analysis and Cohort Retention at Scale

## Computing conversion funnels, retention matrices, and session aggregates incrementally — keeping product analytics live

---

Product analytics is dominated by two questions: "where do users drop off?" (funnels) and "do users come back?" (retention). These questions drive product decisions worth millions of dollars. They're also among the most expensive queries to compute at scale.

A conversion funnel scans every user's event history to determine how far they progressed through a sequence of steps. A retention matrix examines every user's activity across multiple time periods. For a product with 10 million monthly active users generating 500 million events per month, these queries read hundreds of millions of rows and take minutes to complete.

And yet the answers change slowly. Of those 500 million events, only the ones generated in the last few minutes are new. The funnel for users who signed up last week hasn't changed — those users' journeys are already complete. The retention for January's cohort was fixed by the end of February. Only the current period's numbers are actively evolving.

pg_trickle exploits this property. By maintaining funnel and retention analytics as stream tables, only new events trigger updates. Historical cohorts are untouched. The live analytics dashboard stays current within seconds, even as the total event volume reaches billions.

---

## The Classic Funnel Query

A typical e-commerce funnel tracks users through: Visit → Product View → Add to Cart → Checkout → Purchase. The SQL looks like:

```sql
SELECT
    date_trunc('week', first_visit) AS cohort_week,
    COUNT(DISTINCT user_id) AS visitors,
    COUNT(DISTINCT user_id) FILTER (WHERE viewed_product) AS product_viewers,
    COUNT(DISTINCT user_id) FILTER (WHERE added_to_cart) AS cart_adders,
    COUNT(DISTINCT user_id) FILTER (WHERE started_checkout) AS checkout_starters,
    COUNT(DISTINCT user_id) FILTER (WHERE completed_purchase) AS purchasers
FROM (
    SELECT
        user_id,
        MIN(created_at) FILTER (WHERE event_type = 'page_visit') AS first_visit,
        bool_or(event_type = 'product_view') AS viewed_product,
        bool_or(event_type = 'add_to_cart') AS added_to_cart,
        bool_or(event_type = 'checkout_start') AS started_checkout,
        bool_or(event_type = 'purchase') AS completed_purchase
    FROM events
    GROUP BY user_id
) user_journeys
GROUP BY date_trunc('week', first_visit);
```

This query reads the entire events table, computes per-user journey completions, and aggregates by cohort. For 500 million events, it's a multi-minute query. Run it every time a product manager opens the dashboard and you're spending significant database resources on repetitive computation.

---

## Funnel as a Stream Table

Break it into two layers — user journey state and cohort aggregation:

```sql
-- Layer 1: Per-user funnel progression
SELECT pgtrickle.create_stream_table(
    'user_funnel_state',
    $$
    SELECT
        user_id,
        MIN(created_at) FILTER (WHERE event_type = 'page_visit') AS first_visit,
        bool_or(event_type = 'page_visit') AS visited,
        bool_or(event_type = 'product_view') AS viewed_product,
        bool_or(event_type = 'add_to_cart') AS added_to_cart,
        bool_or(event_type = 'checkout_start') AS started_checkout,
        bool_or(event_type = 'purchase') AS completed_purchase
    FROM events
    GROUP BY user_id
    $$
);
```

When a user triggers an `add_to_cart` event, only that user's row in `user_funnel_state` is updated. The `added_to_cart` flag flips from false to true. The other 10 million users' states are untouched.

```sql
-- Layer 2: Cohort aggregation
SELECT pgtrickle.create_stream_table(
    'weekly_funnel',
    $$
    SELECT
        date_trunc('week', first_visit) AS cohort_week,
        COUNT(*) AS total_users,
        COUNT(*) FILTER (WHERE viewed_product) AS product_viewers,
        COUNT(*) FILTER (WHERE added_to_cart) AS cart_adders,
        COUNT(*) FILTER (WHERE started_checkout) AS checkout_starters,
        COUNT(*) FILTER (WHERE completed_purchase) AS purchasers
    FROM user_funnel_state
    WHERE first_visit IS NOT NULL
    GROUP BY date_trunc('week', first_visit)
    $$
);
```

The cohort aggregation reads from the user funnel state (not from raw events). When one user's state changes, only their cohort's counts are adjusted. If a user from the March 15 cohort completes a purchase, the `purchasers` count for that week increments by 1. All other weeks are untouched.

The cascade processes: one new event → one user state update → one cohort count adjustment. Total rows processed: 3, regardless of whether you have 1 million or 1 billion historical events.

---

## Retention Matrices

Retention analysis asks: of the users who signed up in week W, what fraction were active in week W+1, W+2, W+3, etc.?

```sql
-- User-week activity matrix
SELECT pgtrickle.create_stream_table(
    'user_weekly_activity',
    $$
    SELECT
        user_id,
        date_trunc('week', MIN(created_at)) AS signup_week,
        date_trunc('week', created_at) AS active_week,
        COUNT(*) AS event_count
    FROM events
    GROUP BY user_id, date_trunc('week', created_at)
    $$
);
```

This stream table maintains one row per user per active week. When a user generates events in a new week, a new row appears. When they generate more events in the same week, the count increments.

The retention matrix is then:

```sql
SELECT pgtrickle.create_stream_table(
    'retention_matrix',
    $$
    SELECT
        signup_week,
        (EXTRACT(EPOCH FROM active_week - signup_week) / 604800)::integer AS weeks_since_signup,
        COUNT(DISTINCT user_id) AS active_users
    FROM user_weekly_activity
    GROUP BY signup_week, (EXTRACT(EPOCH FROM active_week - signup_week) / 604800)::integer
    $$
);
```

Each cell in the retention matrix — "signup week X, active in week X+N" — is a distinct count maintained incrementally. When a user from the January cohort is active in their 8th week, only the (January, +8) cell increments. The other hundreds of cells in the matrix are untouched.

This is dramatic efficiency for mature products. A product with 2 years of weekly cohorts has 104 × 104 = 10,816 cells in the full retention matrix. But in any given week, only the "current week" column changes (existing users becoming active this week). That's at most 104 cell updates. The other 10,712 cells represent historical retention that will never change again.

---

## Session-Based Funnels

Some funnels are per-session rather than per-user lifetime. "In a single session, how many users go from landing page to signup?"

```sql
-- Session boundaries (30-minute gap = new session)
SELECT pgtrickle.create_stream_table(
    'session_funnels',
    $$
    WITH sessions AS (
        SELECT
            user_id,
            created_at,
            event_type,
            SUM(CASE WHEN created_at - lag_ts > interval '30 minutes' THEN 1 ELSE 0 END)
                OVER (PARTITION BY user_id ORDER BY created_at) AS session_id
        FROM (
            SELECT *,
                LAG(created_at) OVER (PARTITION BY user_id ORDER BY created_at) AS lag_ts
            FROM events
        ) e
    )
    SELECT
        date_trunc('day', MIN(created_at)) AS day,
        user_id,
        session_id,
        bool_or(event_type = 'landing_page') AS saw_landing,
        bool_or(event_type = 'signup_form') AS saw_signup_form,
        bool_or(event_type = 'signup_complete') AS completed_signup
    FROM sessions
    GROUP BY user_id, session_id
    $$
);
```

Each new event is assigned to a session (based on the 30-minute gap heuristic) and the session's funnel state is updated. Sessions that ended hours ago are never re-examined.

---

## Conversion Rate Over Time

The most actionable metric is often the conversion rate trend — is it improving or declining?

```sql
SELECT pgtrickle.create_stream_table(
    'daily_conversion_rates',
    $$
    SELECT
        date_trunc('day', first_visit) AS day,
        COUNT(*) AS total_visitors,
        COUNT(*) FILTER (WHERE completed_purchase) AS purchasers,
        COUNT(*) FILTER (WHERE completed_purchase)::float / NULLIF(COUNT(*), 0) AS conversion_rate
    FROM user_funnel_state
    WHERE first_visit IS NOT NULL
    GROUP BY date_trunc('day', first_visit)
    $$
);
```

This gives you a live conversion rate per day, updated incrementally. When a user who visited today completes a purchase (possibly hours later), today's conversion rate ticks up. Product managers can watch the conversion rate move in real time during an A/B test launch, without waiting for a nightly analytics rebuild.

---

## Segmented Funnels

Real product analytics always slices by dimensions — device type, acquisition channel, geographic region, user plan:

```sql
SELECT pgtrickle.create_stream_table(
    'funnel_by_channel',
    $$
    SELECT
        u.acquisition_channel,
        date_trunc('week', ufs.first_visit) AS cohort_week,
        COUNT(*) AS visitors,
        COUNT(*) FILTER (WHERE ufs.viewed_product) AS viewers,
        COUNT(*) FILTER (WHERE ufs.completed_purchase) AS purchasers
    FROM user_funnel_state ufs
    JOIN users u ON u.id = ufs.user_id
    GROUP BY u.acquisition_channel, date_trunc('week', ufs.first_visit)
    $$
);
```

The join with the users table brings in segmentation dimensions. When a user's funnel state changes, their segment's counts are adjusted. When a user's segment changes (e.g., they upgrade from "free" to "paid"), both the old and new segment's counts are adjusted.

---

## Replacing Amplitude / Mixpanel / PostHog Analytics

Product analytics SaaS tools charge per event volume. At scale (hundreds of millions of events per month), this becomes expensive. More importantly, your data lives in a third-party system — you can't join it with operational data, run arbitrary queries, or maintain custom metrics.

With pg_trickle, product analytics is just SQL:

| Feature | SaaS analytics | pg_trickle stream tables |
|---------|---------------|--------------------------|
| Funnel computation | Proprietary query engine | Standard SQL (GROUP BY, FILTER) |
| Retention matrices | Pre-built visualization | SQL query → dashboard tool |
| Real-time updates | Minutes of delay | Seconds (refresh interval) |
| Custom metrics | Limited to tool's model | Arbitrary SQL |
| Data residency | Vendor's cloud | Your PostgreSQL instance |
| Cost model | Per event ($$$) | Per compute (database cost) |
| Joins with business data | Export/import | Direct JOIN in same database |

The trade-off is visualization. SaaS tools provide beautiful funnel charts and retention heatmaps out of the box. With pg_trickle, you connect a visualization tool (Grafana, Metabase, Superset) to the stream tables and build your own dashboards. The data is always fresh — the visualization layer just reads from pre-computed tables.

---

## Performance at Scale

For a product with 10M MAU and 500M events/month:

| Query | Full computation | Incremental update |
|-------|-----------------|-------------------|
| Weekly funnel (all users) | 45s | 3ms (per event batch) |
| Retention matrix (104 weeks) | 2.5min | 1ms (per active user) |
| Daily conversion rate | 12s | <1ms (per user state change) |

The incremental cost is per-event or per-user-state-change — constant regardless of historical data volume. Your product analytics stay responsive as your user base grows from 100K to 10M, without upgrading your analytics infrastructure.

---

*Stop computing funnels from scratch. Let the differential engine maintain your product analytics incrementally — fresh conversion rates, live retention matrices, and instant cohort insights without billion-row scans.*
