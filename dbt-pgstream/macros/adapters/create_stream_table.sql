{#
  pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize)

  Creates a new stream table via pgstream.create_stream_table().
  Called by the stream_table materialization on first run.

  Args:
    name (str): Stream table name (may be schema-qualified)
    query (str): The defining SQL query
    schedule (str|none): Refresh schedule (e.g., '1m', '5m', '0 */2 * * *').
                         Pass none for pg_stream's CALCULATED schedule (SQL NULL).
    refresh_mode (str): 'FULL' or 'DIFFERENTIAL'
    initialize (bool): Whether to populate immediately on creation
#}
{% macro pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize) %}
  {% set create_sql %}
    SELECT pgstream.create_stream_table(
      {{ dbt.string_literal(name) }},
      {{ dbt.string_literal(query) }},
      {% if schedule is none %}NULL{% else %}{{ dbt.string_literal(schedule) }}{% endif %},
      {{ dbt.string_literal(refresh_mode) }},
      {{ initialize }}
    )
  {% endset %}
  {% do run_query(create_sql) %}
  {{ log("pg_stream: created stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
