-- Q0: Passthrough — identity projection of all bids
-- Tests: basic scan, no transformation
SELECT bid_id, auction, bidder, price, channel, url, date_time
FROM bid
