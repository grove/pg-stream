-- =============================================================================
-- Nexmark RF2: Bulk DELETE — simulate expired/cancelled events
--
-- Deletes bids first (FK constraint), then auctions, then persons.
-- __RF_COUNT__ is replaced at runtime.
-- =============================================================================

-- Delete oldest bids (by bid_id)
DELETE FROM bid
WHERE bid_id IN (
    SELECT bid_id FROM bid ORDER BY bid_id LIMIT __RF_COUNT__
);

-- Delete auctions that no longer have any bids and are among the oldest
DELETE FROM auction
WHERE id IN (
    SELECT a.id
    FROM auction a
    LEFT JOIN bid b ON b.auction = a.id
    WHERE b.bid_id IS NULL
    ORDER BY a.id
    LIMIT (__RF_COUNT__ / 5 + 1)
);

-- Delete persons who have no auctions and no bids (clean orphans)
DELETE FROM person
WHERE id IN (
    SELECT p.id
    FROM person p
    LEFT JOIN auction a ON a.seller = p.id
    LEFT JOIN bid b ON b.bidder = p.id
    WHERE a.id IS NULL AND b.bid_id IS NULL
    ORDER BY p.id
    LIMIT (__RF_COUNT__ / 10 + 1)
);
