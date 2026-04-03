-- =============================================================================
-- Nexmark RF3: Targeted UPDATEs — simulate price changes and profile edits
--
-- Updates bid prices, auction reserves, and person cities.
-- __RF_COUNT__ is replaced at runtime.
-- =============================================================================

-- Update bid prices (raise by a random offset)
UPDATE bid
SET price = price + (bid_id % 100) + 1
WHERE bid_id IN (
    SELECT bid_id FROM bid ORDER BY bid_id DESC LIMIT __RF_COUNT__
);

-- Update auction reserves (lower reserve to attract more bids)
UPDATE auction
SET reserve = GREATEST(initial_bid, reserve - (id % 50) - 1)
WHERE id IN (
    SELECT id FROM auction ORDER BY id DESC LIMIT (__RF_COUNT__ / 3 + 1)
);

-- Update person cities (simulate moves)
UPDATE person
SET city = (ARRAY['Miami', 'Atlanta', 'Dallas', 'Detroit', 'Minneapolis'])[1 + (id % 5)]
WHERE id IN (
    SELECT id FROM person ORDER BY id DESC LIMIT (__RF_COUNT__ / 5 + 1)
);
