-- Q5: Hot items — count bids per auction
-- Tests: GROUP BY + COUNT + ORDER BY
-- Simplified from the original window-based Q5 to a cumulative count
-- (stream tables maintain running state, not time-windowed snapshots).
SELECT
    auction,
    COUNT(*) AS num_bids
FROM bid
GROUP BY auction
