{{ config(
    materialized='stream_table',
    schedule='1m',
    refresh_mode='DIFFERENTIAL',
    partition_by='customer_id'
) }}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
