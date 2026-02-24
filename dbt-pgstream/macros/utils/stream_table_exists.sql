{#
  pgstream_stream_table_exists(name)

  Checks if a stream table exists in the pg_stream catalog.
  Returns true/false. Handles both schema-qualified and unqualified names.

  Args:
    name (str): Stream table name. May be schema-qualified ('analytics.order_totals')
                or unqualified ('order_totals' — defaults to target.schema).
#}
{% macro pgstream_stream_table_exists(name) %}
  {% if execute %}
    {# Split schema-qualified name if present #}
    {% set parts = name.split('.') %}
    {% if parts | length == 2 %}
      {% set lookup_schema = parts[0] %}
      {% set lookup_name = parts[1] %}
    {% else %}
      {% set lookup_schema = target.schema %}
      {% set lookup_name = name %}
    {% endif %}

    {% set query %}
      SELECT EXISTS(
        SELECT 1 FROM pgstream.pgs_stream_tables
        WHERE pgs_schema = {{ dbt.string_literal(lookup_schema) }}
          AND pgs_name = {{ dbt.string_literal(lookup_name) }}
      ) AS st_exists
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows %}
      {{ return(result.rows[0]['st_exists']) }}
    {% endif %}
  {% endif %}
  {{ return(false) }}
{% endmacro %}
