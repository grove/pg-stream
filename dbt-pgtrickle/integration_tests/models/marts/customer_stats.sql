{{ config(
    materialized='stream_table',
    schedule='2m',
    refresh_mode='DIFFERENTIAL'
) }}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count,
    AVG(amount) AS avg_amount
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
