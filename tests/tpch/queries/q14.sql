-- Q14: Promotion Effect
-- Operators: 2-table Join → Conditional SUM ratio
SELECT
    100.00 * SUM(CASE
        WHEN p_type LIKE 'PROMO%'
        THEN l_extendedprice * (1 - l_discount)
        ELSE 0
    END) / NULLIF(SUM(l_extendedprice * (1 - l_discount)), 0) AS promo_revenue
FROM lineitem, part
WHERE l_partkey = p_partkey
  AND l_shipdate >= DATE '1995-09-01'
  AND l_shipdate < DATE '1995-09-01' + INTERVAL '1 month'
