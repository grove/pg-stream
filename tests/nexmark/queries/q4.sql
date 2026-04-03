-- Q4: Average selling price by category
-- Tests: JOIN + GROUP BY + aggregate (AVG)
-- Note: uses a subquery to find the winning (max) bid per auction,
-- then aggregates by category.
SELECT
    a.category,
    AVG(b.max_price) AS avg_price
FROM auction a
JOIN (
    SELECT auction, MAX(price) AS max_price
    FROM bid
    GROUP BY auction
) b ON a.id = b.auction
GROUP BY a.category
