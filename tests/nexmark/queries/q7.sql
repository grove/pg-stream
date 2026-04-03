-- Q7: Highest bid — max bid price overall
-- Tests: aggregate (MAX) on full table
-- Simplified from the original time-windowed version.
SELECT MAX(price) AS max_price
FROM bid
