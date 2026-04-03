-- Q6: Average selling price per seller
-- Tests: JOIN + GROUP BY + AVG (multi-table)
SELECT
    a.seller,
    AVG(b.price) AS avg_bid_price,
    COUNT(*) AS total_bids
FROM bid b
JOIN auction a ON b.auction = a.id
GROUP BY a.seller
