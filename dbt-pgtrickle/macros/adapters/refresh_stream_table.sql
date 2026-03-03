{#
  pgtrickle_refresh_stream_table(name)

  Triggers an immediate refresh of a stream table via pgtrickle.refresh_stream_table().

  Args:
    name (str): Stream table name (schema-qualified)
#}
{% macro pgtrickle_refresh_stream_table(name) %}
  {#
    Run refresh_stream_table() outside dbt's model transaction — see
    pgtrickle_create_stream_table for the full rationale (explicit BEGIN/COMMIT).
  #}
  {% call statement('pgtrickle_refresh', auto_begin=False, fetch_result=False) %}
    BEGIN;
    SELECT pgtrickle.refresh_stream_table({{ dbt.string_literal(name) }});
    COMMIT;
  {% endcall %}
  {{ log("pg_trickle: refreshed stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
