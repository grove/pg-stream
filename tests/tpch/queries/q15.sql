-- Q15: Top Supplier
-- Operators: Derived-table Join -> Scalar Subquery (MAX) -> Filter
-- CTE replaced with inline derived table (pg_stream does not support WITH CTEs).
SELECT
    s_suppkey,
    s_name,
    s_address,
    s_phone,
    revenue0.total_revenue
FROM supplier,
     (
         SELECT
             l_suppkey AS supplier_no,
             SUM(l_extendedprice * (1 - l_discount)) AS total_revenue
         FROM lineitem
         WHERE l_shipdate >= DATE '1996-01-01'
           AND l_shipdate < DATE '1996-01-01' + INTERVAL '3 months'
         GROUP BY l_suppkey
     ) AS revenue0
WHERE s_suppkey = supplier_no
  AND total_revenue = (
      SELECT MAX(q.total_revenue)
      FROM (
          SELECT SUM(l_extendedprice * (1 - l_discount)) AS total_revenue
          FROM lineitem
          WHERE l_shipdate >= DATE '1996-01-01'
            AND l_shipdate < DATE '1996-01-01' + INTERVAL '3 months'
          GROUP BY l_suppkey
      ) q
  )
