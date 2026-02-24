{#
  pgstream_get_stream_table_info(name)

  Returns metadata for a stream table from the pg_stream catalog.
  Returns a row dict with pgs_name, pgs_schema, defining_query, schedule,
  refresh_mode, status — or none if the stream table does not exist.

  Args:
    name (str): Stream table name. May be schema-qualified ('analytics.order_totals')
                or unqualified ('order_totals' — defaults to target.schema).
#}
{% macro pgstream_get_stream_table_info(name) %}
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
      SELECT pgs_name, pgs_schema, defining_query, schedule, refresh_mode, status
      FROM pgstream.pgs_stream_tables
      WHERE pgs_schema = {{ dbt.string_literal(lookup_schema) }}
        AND pgs_name = {{ dbt.string_literal(lookup_name) }}
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {{ return(result.rows[0]) }}
    {% endif %}
  {% endif %}
  {{ return(none) }}
{% endmacro %}
