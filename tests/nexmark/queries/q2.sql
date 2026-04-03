-- Q2: Selection — filter bids by specific auctions
-- Tests: WHERE filter
SELECT auction, price, bid_id
FROM bid
WHERE auction IN (1007, 1020, 2001, 2019, 2087)
   OR (auction >= 1000 AND auction <= 1010)
