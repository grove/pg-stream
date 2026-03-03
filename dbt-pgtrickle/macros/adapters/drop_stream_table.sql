{#
  pgtrickle_drop_stream_table(name)

  Drops a stream table via pgtrickle.drop_stream_table().
  Called by the materialization on --full-refresh or when the defining query changes.

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgtrickle_drop_stream_table(name) %}
  {#
    Run drop_stream_table() outside dbt's model transaction — see
    pgtrickle_create_stream_table for the full rationale (explicit BEGIN/COMMIT).
  #}
  {% call statement('pgtrickle_drop', auto_begin=False, fetch_result=False) %}
    BEGIN;
    SELECT pgtrickle.drop_stream_table({{ dbt.string_literal(name) }});
    COMMIT;
  {% endcall %}
  {{ log("pg_trickle: dropped stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
