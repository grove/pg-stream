{#
  pgtrickle_refresh_stream_table(name)

  Triggers an immediate refresh of a stream table via pgtrickle.refresh_stream_table().

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgtrickle_refresh_stream_table(name) %}
  {% set refresh_sql %}
    SELECT pgtrickle.refresh_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(refresh_sql) %}
  {{ log("pg_trickle: refreshed stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
