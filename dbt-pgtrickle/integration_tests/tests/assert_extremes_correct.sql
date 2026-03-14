-- Verify order_extremes stream table matches expected aggregation.
-- Returns rows that are in expected but missing/different in actual.
-- An empty result set means the test passes.
WITH expected AS (
    SELECT
        customer_id,
        MAX(amount) AS max_amount,
        MIN(amount) AS min_amount
    FROM {{ ref('raw_orders') }}
    GROUP BY customer_id
),
actual AS (
    SELECT customer_id, max_amount, min_amount
    FROM {{ ref('order_extremes') }}
)
SELECT e.*
FROM expected e
LEFT JOIN actual a
  ON e.customer_id = a.customer_id
  AND e.max_amount = a.max_amount
  AND e.min_amount = a.min_amount
WHERE a.customer_id IS NULL
