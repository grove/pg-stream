{#
  pgstream_refresh_stream_table(name)

  Triggers an immediate refresh of a stream table via pgstream.refresh_stream_table().

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgstream_refresh_stream_table(name) %}
  {% set refresh_sql %}
    SELECT pgstream.refresh_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(refresh_sql) %}
  {{ log("pg_stream: refreshed stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
