-- Q14: Promotion Effect
-- Operators: 2-table Join -> Conditional SUM ratio
SELECT
    100.00 * SUM(CASE
        WHEN left(p_type, 5) = 'PROMO'
        THEN l_extendedprice * (1 - l_discount)
        ELSE 0
    END) /
    CASE WHEN SUM(l_extendedprice * (1 - l_discount)) = 0
         THEN NULL
         ELSE SUM(l_extendedprice * (1 - l_discount))
    END AS promo_revenue
FROM lineitem, part
WHERE l_partkey = p_partkey
  AND l_shipdate >= DATE '1995-09-01'
  AND l_shipdate < DATE '1995-09-01' + INTERVAL '1 month'
