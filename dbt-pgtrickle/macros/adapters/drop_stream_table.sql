{#
  pgtrickle_drop_stream_table(name)

  Drops a stream table via pgtrickle.drop_stream_table().
  Called by the materialization on --full-refresh or when the defining query changes.

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgtrickle_drop_stream_table(name) %}
  {% set drop_sql %}
    SELECT pgtrickle.drop_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(drop_sql) %}
  {{ log("pg_trickle: dropped stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
