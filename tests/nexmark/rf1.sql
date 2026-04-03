-- =============================================================================
-- Nexmark RF1: Bulk INSERT — simulate new events arriving
--
-- Inserts new persons, auctions, and bids. __RF_COUNT__ is replaced at
-- runtime. __NM_NEXT_PERSON__, __NM_NEXT_AUCTION__ mark the start of
-- new ID ranges (above existing data).
-- =============================================================================

-- New persons
INSERT INTO person (id, name, email, city, state, date_time)
SELECT
    __NM_NEXT_PERSON__ + g AS id,
    'new_person_' || (__NM_NEXT_PERSON__ + g) AS name,
    'new_' || (__NM_NEXT_PERSON__ + g) || '@example.com' AS email,
    (ARRAY['Phoenix', 'Los Angeles', 'San Francisco', 'Portland',
           'Seattle', 'New York', 'Chicago', 'Denver', 'Austin',
           'Boston'])[1 + (g % 10)] AS city,
    (ARRAY['AZ', 'CA', 'CA', 'OR', 'WA',
           'NY', 'IL', 'CO', 'TX', 'MA'])[1 + (g % 10)] AS state,
    now() + (g || ' seconds')::interval AS date_time
FROM generate_series(1, (__RF_COUNT__ / 5 + 1)) g;

-- New auctions (using both existing and new sellers)
INSERT INTO auction (id, item_name, description, initial_bid, reserve,
                     seller, category, date_time, expires)
SELECT
    __NM_NEXT_AUCTION__ + g AS id,
    'new_item_' || (__NM_NEXT_AUCTION__ + g) AS item_name,
    'New description ' || g AS description,
    (10 + (g * 7) % 991) AS initial_bid,
    (100 + (g * 13) % 9901) AS reserve,
    -- Mix of existing and newly inserted sellers
    CASE WHEN g % 3 = 0
         THEN __NM_NEXT_PERSON__ + 1 + (g % (__RF_COUNT__ / 5 + 1))
         ELSE 1 + (g % __NM_PERSONS__)
    END AS seller,
    1 + (g % 10) AS category,
    now() + (g || ' seconds')::interval AS date_time,
    now() + '30 days'::interval + (g || ' seconds')::interval AS expires
FROM generate_series(1, (__RF_COUNT__ / 3 + 1)) g;

-- New bids (on both existing and new auctions)
INSERT INTO bid (auction, bidder, price, channel, url, date_time)
SELECT
    CASE WHEN g % 4 = 0
         THEN __NM_NEXT_AUCTION__ + 1 + (g % (__RF_COUNT__ / 3 + 1))
         ELSE 1 + (g % __NM_AUCTIONS__)
    END AS auction,
    1 + (g % __NM_PERSONS__) AS bidder,
    (50 + (g * 11) % 4951) AS price,
    (ARRAY['web', 'mobile', 'api'])[1 + (g % 3)] AS channel,
    'https://example.com/bid/new_' || g AS url,
    now() + (g || ' seconds')::interval AS date_time
FROM generate_series(1, __RF_COUNT__) g;
