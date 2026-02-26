-- RF3: Targeted UPDATEs (extension beyond standard TPC-H).
-- Token replaced by harness: __RF_COUNT__
--
-- Updates non-key and key columns to exercise UPDATE delta paths:
-- - Price changes (non-key, affects SUM/AVG aggregates)
-- - Market segment changes (GROUP BY key, causes group transitions)
-- - Quantity changes (affects filter predicates like l_quantity < threshold)

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

-- Move some customers to a different market segment (GROUP BY key change)
UPDATE customer
SET c_mktsegment = CASE c_mktsegment
        WHEN 'AUTOMOBILE' THEN 'BUILDING'
        WHEN 'BUILDING'   THEN 'FURNITURE'
        WHEN 'FURNITURE'  THEN 'HOUSEHOLD'
        WHEN 'HOUSEHOLD'  THEN 'MACHINERY'
        WHEN 'MACHINERY'  THEN 'AUTOMOBILE'
        ELSE 'BUILDING'
    END
WHERE c_custkey IN (
    SELECT c_custkey FROM customer
    ORDER BY c_custkey DESC
    LIMIT GREATEST(__RF_COUNT__ / 2, 1)
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
