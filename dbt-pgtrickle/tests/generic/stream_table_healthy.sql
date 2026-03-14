{#
  test_stream_table_healthy(model, warn_seconds)

  Generic dbt test that fails if a stream table is not healthy.
  Checks the pg_trickle catalog via pgtrickle_stream_table_status() and returns
  a row for each unhealthy condition found.

  Usage in schema.yml:
    models:
      - name: order_totals
        tests:
          - dbt_pgtrickle.stream_table_healthy:
              warn_seconds: 300  # optional, default 300s (5 min)

  Returns an empty result set if the stream table is healthy (test passes).
  Returns rows if the stream table is stale, erroring, paused, or not found.
#}
{% test stream_table_healthy(model, warn_seconds=300) %}
  {%- set model_name = model.identifier -%}
  {%- set model_schema = model.schema -%}

  SELECT
    pgt_name,
    pgt_schema,
    status,
    EXTRACT(EPOCH FROM staleness)::int AS staleness_seconds,
    consecutive_errors,
    last_refresh_at,
    is_populated,
    CASE
      WHEN status = 'PAUSED' THEN 'paused'
      WHEN consecutive_errors > 0 THEN 'erroring'
      WHEN stale = true THEN 'stale'
      WHEN staleness IS NOT NULL
           AND EXTRACT(EPOCH FROM staleness) > {{ warn_seconds }}
      THEN 'stale'
      ELSE 'healthy'
    END AS health_status
  FROM pgtrickle.pg_stat_stream_tables
  WHERE pgt_schema = {{ dbt.string_literal(model_schema) }}
    AND pgt_name = {{ dbt.string_literal(model_name) }}
    AND (
      status = 'PAUSED'
      OR consecutive_errors > 0
      OR stale = true
      OR (staleness IS NOT NULL AND EXTRACT(EPOCH FROM staleness) > {{ warn_seconds }})
    )
{% endtest %}
