-- Q16: Parts/Supplier Relationship
-- Operators: 3-table Join -> NOT IN subquery -> COUNT (DISTINCT rewritten as subquery)
-- COUNT(DISTINCT ps_suppkey) rewritten as DISTINCT subquery + COUNT(*) to avoid
-- pg_stream DIFFERENTIAL limitation with DISTINCT aggregates.
-- NOT LIKE rewritten with left()/strpos to avoid A_Expr kind 7 (LIKE).
SELECT
    p_brand,
    p_type,
    p_size,
    COUNT(*) AS supplier_cnt
FROM (
    SELECT DISTINCT p_brand, p_type, p_size, ps_suppkey
    FROM partsupp, part
    WHERE p_partkey = ps_partkey
      AND p_brand <> 'Brand#45'
      AND left(p_type, 15) <> 'MEDIUM POLISHED'
      AND p_size IN (49, 14, 23, 45, 19, 3, 36, 9)
      AND ps_suppkey NOT IN (
          SELECT s_suppkey
          FROM supplier
          WHERE strpos(s_comment, 'Customer') > 0
            AND strpos(s_comment, 'Complaints') > 0
      )
) _dedup
GROUP BY p_brand, p_type, p_size
