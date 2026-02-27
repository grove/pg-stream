-- Q13: Customer Distribution
-- Operators: LEFT JOIN -> Subquery-in-FROM -> Aggregate
SELECT
    c_count,
    COUNT(*) AS custdist
FROM (
    SELECT
        c_custkey,
        COUNT(o_orderkey) AS c_count
    FROM customer LEFT OUTER JOIN orders ON c_custkey = o_custkey
        AND o_comment NOT LIKE '%special%requests%'
    GROUP BY c_custkey
) AS c_orders
GROUP BY c_count
