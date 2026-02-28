{#
  pgtrickle_get_stream_table_info(name)

  Returns metadata for a stream table from the pg_trickle catalog.
  Returns a row dict with pgt_name, pgt_schema, defining_query, schedule,
  refresh_mode, status — or none if the stream table does not exist.

  Args:
    name (str): Stream table name. May be schema-qualified ('analytics.order_totals')
                or unqualified ('order_totals' — defaults to target.schema).
#}
{% macro pgtrickle_get_stream_table_info(name) %}
  {% if execute %}
    {% set parts = name.split('.') %}
    {% if parts | length == 2 %}
      {% set lookup_schema = parts[0] %}
      {% set lookup_name = parts[1] %}
    {% else %}
      {% set lookup_schema = target.schema %}
      {% set lookup_name = name %}
    {% endif %}

    {% set query %}
      SELECT pgt_name, pgt_schema, defining_query, schedule, refresh_mode, status
      FROM pgtrickle.pgt_stream_tables
      WHERE pgt_schema = {{ dbt.string_literal(lookup_schema) }}
        AND pgt_name = {{ dbt.string_literal(lookup_name) }}
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {{ return(result.rows[0]) }}
    {% endif %}
  {% endif %}
  {{ return(none) }}
{% endmacro %}
