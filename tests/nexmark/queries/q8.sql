-- Q8: Monitor new users — persons who created auctions
-- Tests: JOIN between person and auction
SELECT
    p.id AS person_id,
    p.name,
    a.id AS auction_id,
    a.reserve
FROM person p
JOIN auction a ON p.id = a.seller
