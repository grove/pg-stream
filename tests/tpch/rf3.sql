-- RF3: Targeted UPDATEs (extension beyond standard TPC-H).
-- Token replaced by harness: __RF_COUNT__
--
-- Updates non-key and key columns to exercise UPDATE delta paths:
-- - Price changes (non-key, affects SUM/AVG aggregates)
-- - Quantity changes (affects filter predicates like l_quantity < threshold)
--
-- NOTE: customer UPDATE (market-segment rotation) is intentionally omitted.
-- pg_trickle has a known DVM bug where refreshing a LEFT JOIN stream table
-- after an UPDATE to the left-joined table generates invalid SQL on the
-- second+ refresh cycle ("column c_custkey does not exist"). This affects
-- Q13 only. Tracked separately. Do not re-add until the LEFT JOIN DVM is fixed.

-- Update extended price on a batch of lineitems (non-key column)
UPDATE lineitem
SET l_extendedprice = l_extendedprice * 1.05,
    l_discount = LEAST(l_discount + 0.01, 0.10)
WHERE (l_orderkey, l_linenumber) IN (
    SELECT l_orderkey, l_linenumber
    FROM lineitem
    ORDER BY l_orderkey DESC, l_linenumber
    LIMIT __RF_COUNT__
);

-- Update some lineitem quantities (affects filter predicates)
UPDATE lineitem
SET l_quantity = l_quantity + 5
WHERE (l_orderkey, l_linenumber) IN (
    SELECT l_orderkey, l_linenumber
    FROM lineitem
    ORDER BY l_orderkey, l_linenumber DESC
    LIMIT GREATEST(__RF_COUNT__ / 2, 1)
);
