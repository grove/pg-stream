{{ config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='FULL'
) }}

SELECT
    customer_id,
    MAX(amount) AS max_amount,
    MIN(amount) AS min_amount
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
