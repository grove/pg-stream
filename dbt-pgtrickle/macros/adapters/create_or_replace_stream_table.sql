{#
  pgtrickle_create_or_replace_stream_table(name, query, schedule, refresh_mode, initialize, cdc_mode, partition_by)

  Creates or replaces a stream table via pgtrickle.create_or_replace_stream_table().
  Idempotent: creates if absent, no-ops if identical, alters if config/query changed.

  Args:
    name (str): Stream table name (may be schema-qualified)
    query (str): The defining SQL query
    schedule (str|none): Refresh schedule (e.g., '1m', '5m', '0 */2 * * *').
                         Pass none for pg_trickle's CALCULATED schedule (SQL NULL).
    refresh_mode (str): 'AUTO', 'FULL', 'DIFFERENTIAL', or 'IMMEDIATE'
    initialize (bool): Whether to populate immediately on creation (only used on first create)
    cdc_mode (str|none): Optional CDC mode override ('auto', 'trigger', 'wal')
    partition_by (str|none): Optional column name to partition the storage table by (RANGE).
                             Only applied on creation; ignored if the stream table already exists.
#}
{% macro pgtrickle_create_or_replace_stream_table(name, query, schedule, refresh_mode, initialize, cdc_mode=none, partition_by=none) %}
  {#
    Run create_or_replace_stream_table() outside of dbt's model transaction.
    dbt wraps the model's main statement in BEGIN...ROLLBACK (for testing /
    dry-run purposes). Any SQL executed via run_query() shares the same
    connection and is therefore also rolled back. To prevent this, we embed
    explicit BEGIN / COMMIT so the DDL commits unconditionally.
  #}
  {% call statement('pgtrickle_create_or_replace', auto_begin=False, fetch_result=False) %}
    BEGIN;
    SELECT pgtrickle.create_or_replace_stream_table(
      {{ dbt.string_literal(name) }},
      $pgtrickle${{ query }}$pgtrickle$,
      {% if schedule is none %}'calculated'{% else %}{{ dbt.string_literal(schedule) }}{% endif %},
      {{ dbt.string_literal(refresh_mode) }},
      {{ initialize }},
      NULL,
      NULL,
      {% if cdc_mode is none %}NULL{% else %}{{ dbt.string_literal(cdc_mode) }}{% endif %},
      false,
      false,
      {% if partition_by is none %}NULL{% else %}{{ dbt.string_literal(partition_by) }}{% endif %}
    );
    COMMIT;
  {% endcall %}
  {{ log("pg_trickle: create_or_replace stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
