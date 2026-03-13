{#
  pgtrickle_create_stream_table(name, query, schedule, refresh_mode, initialize, cdc_mode)

  Creates a new stream table via pgtrickle.create_stream_table().
  Called by the stream_table materialization on first run.

  Args:
    name (str): Stream table name (may be schema-qualified)
    query (str): The defining SQL query
    schedule (str|none): Refresh schedule (e.g., '1m', '5m', '0 */2 * * *').
                         Pass none for pg_trickle's CALCULATED schedule (SQL NULL).
    refresh_mode (str): 'FULL' or 'DIFFERENTIAL'
    initialize (bool): Whether to populate immediately on creation
#}
{% macro pgtrickle_create_stream_table(name, query, schedule, refresh_mode, initialize, cdc_mode=none) %}
  {#
    Run create_stream_table() outside of dbt's model transaction.
    dbt wraps the model's main statement in BEGIN...ROLLBACK (for testing /
    dry-run purposes). Any SQL executed via run_query() shares the same
    connection and is therefore also rolled back. To prevent this, we embed
    explicit BEGIN / COMMIT so the DDL commits unconditionally.
  #}
  {% call statement('pgtrickle_create', auto_begin=False, fetch_result=False) %}
    BEGIN;
    SELECT pgtrickle.create_stream_table(
      {{ dbt.string_literal(name) }},
      $pgtrickle${{ query }}$pgtrickle$,
      {% if schedule is none %}'calculated'{% else %}{{ dbt.string_literal(schedule) }}{% endif %},
      {{ dbt.string_literal(refresh_mode) }},
      {{ initialize }},
      NULL,
      NULL,
      {% if cdc_mode is none %}NULL{% else %}{{ dbt.string_literal(cdc_mode) }}{% endif %}
    );
    COMMIT;
  {% endcall %}
  {{ log("pg_trickle: created stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
