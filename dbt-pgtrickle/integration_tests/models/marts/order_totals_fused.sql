{{ config(
    materialized='stream_table',
    schedule='1m',
    refresh_mode='DIFFERENTIAL',
    fuse='auto',
    fuse_ceiling=100000,
    fuse_sensitivity=3
) }}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
