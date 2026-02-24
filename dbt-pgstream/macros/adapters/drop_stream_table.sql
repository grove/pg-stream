{#
  pgstream_drop_stream_table(name)

  Drops a stream table via pgstream.drop_stream_table().
  Called by the materialization on --full-refresh or when the defining query changes.

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgstream_drop_stream_table(name) %}
  {% set drop_sql %}
    SELECT pgstream.drop_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(drop_sql) %}
  {{ log("pg_stream: dropped stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
