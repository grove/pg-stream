-- Q1: Currency conversion — projection with arithmetic
-- Tests: column projection, arithmetic expression
SELECT
    bid_id,
    auction,
    bidder,
    (price * 908) / 1000 AS price_euro,
    channel,
    url,
    date_time
FROM bid
