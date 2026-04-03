-- Q9: Winning bid per auction (highest bid)
-- Tests: Correlated subquery via JOIN + GROUP BY + MAX
-- For each auction, find the highest bid and join with person info.
SELECT
    a.id AS auction_id,
    a.item_name,
    a.category,
    a.seller,
    wb.bidder,
    wb.price AS winning_price,
    p.name AS bidder_name,
    p.city AS bidder_city
FROM auction a
JOIN (
    SELECT DISTINCT ON (auction)
        auction, bidder, price
    FROM bid
    ORDER BY auction, price DESC
) wb ON a.id = wb.auction
JOIN person p ON wb.bidder = p.id
