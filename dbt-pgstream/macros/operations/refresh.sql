{#
  pgstream_refresh(model_name, schema)

  Triggers an immediate refresh of a stream table.
  Schema-qualifies the name using target.schema if not already qualified.

  Usage:
    dbt run-operation pgstream_refresh --args '{"model_name": "order_totals"}'
    dbt run-operation pgstream_refresh --args '{"model_name": "analytics.order_totals"}'

  Args:
    model_name (str): Stream table name (qualified or unqualified)
    schema (str|none): Explicit schema override. Defaults to target.schema.
#}
{% macro pgstream_refresh(model_name, schema=none) %}
  {# Schema-qualify if not already qualified #}
  {% if '.' in model_name %}
    {% set qualified = model_name %}
  {% elif schema is not none %}
    {% set qualified = schema ~ '.' ~ model_name %}
  {% else %}
    {% set qualified = target.schema ~ '.' ~ model_name %}
  {% endif %}
  {{ pgstream_refresh_stream_table(qualified) }}
{% endmacro %}
