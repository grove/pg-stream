-- =============================================================================
-- Nexmark Streaming Benchmark — Data Generator
--
-- Pure-SQL data generation using generate_series. No external tools required.
-- Scale tokens (__NM_PERSONS__, __NM_AUCTIONS__, __NM_BIDS__) are replaced
-- at runtime by the Rust test harness.
-- =============================================================================

-- ── Persons ─────────────────────────────────────────────────────────────
INSERT INTO person (id, name, email, city, state, date_time)
SELECT
    g AS id,
    'person_' || g AS name,
    'person_' || g || '@example.com' AS email,
    (ARRAY['Phoenix', 'Los Angeles', 'San Francisco', 'Portland',
           'Seattle', 'New York', 'Chicago', 'Denver', 'Austin',
           'Boston'])[1 + (g % 10)] AS city,
    (ARRAY['AZ', 'CA', 'CA', 'OR', 'WA',
           'NY', 'IL', 'CO', 'TX', 'MA'])[1 + (g % 10)] AS state,
    '2026-01-01'::timestamptz + (g || ' seconds')::interval AS date_time
FROM generate_series(1, __NM_PERSONS__) g;

-- ── Auctions ────────────────────────────────────────────────────────────
INSERT INTO auction (id, item_name, description, initial_bid, reserve,
                     seller, category, date_time, expires)
SELECT
    g AS id,
    'item_' || g AS item_name,
    'Description for item ' || g AS description,
    (10 + (g * 7) % 991) AS initial_bid,
    (100 + (g * 13) % 9901) AS reserve,
    1 + (g % __NM_PERSONS__) AS seller,
    1 + (g % 10) AS category,
    '2026-01-01'::timestamptz + (g || ' seconds')::interval AS date_time,
    '2026-02-01'::timestamptz + (g || ' seconds')::interval AS expires
FROM generate_series(1, __NM_AUCTIONS__) g;

-- ── Bids ────────────────────────────────────────────────────────────────
INSERT INTO bid (auction, bidder, price, channel, url, date_time)
SELECT
    1 + (g % __NM_AUCTIONS__) AS auction,
    1 + (g % __NM_PERSONS__)  AS bidder,
    (50 + (g * 11) % 4951) AS price,
    (ARRAY['web', 'mobile', 'api'])[1 + (g % 3)] AS channel,
    'https://example.com/bid/' || g AS url,
    '2026-01-01'::timestamptz + (g || ' seconds')::interval AS date_time
FROM generate_series(1, __NM_BIDS__) g;
