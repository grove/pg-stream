{{ config(
    materialized='stream_table',
    schedule='2m',
    refresh_mode='AUTO'
) }}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
