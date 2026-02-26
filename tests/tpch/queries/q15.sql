-- Q15: Top Supplier
-- Operators: CTE (inlined from CREATE VIEW) → Scalar Subquery (MAX) → Filter
-- Original Q15 uses CREATE VIEW revenue0; inlined as CTE for pg_stream.
WITH revenue0 AS (
    SELECT
        l_suppkey AS supplier_no,
        SUM(l_extendedprice * (1 - l_discount)) AS total_revenue
    FROM lineitem
    WHERE l_shipdate >= DATE '1996-01-01'
      AND l_shipdate < DATE '1996-01-01' + INTERVAL '3 months'
    GROUP BY l_suppkey
)
SELECT
    s_suppkey,
    s_name,
    s_address,
    s_phone,
    total_revenue
FROM supplier, revenue0
WHERE s_suppkey = supplier_no
  AND total_revenue = (
      SELECT MAX(total_revenue) FROM revenue0
  )
