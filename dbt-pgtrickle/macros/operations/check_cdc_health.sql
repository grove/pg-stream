{#
  pgtrickle_check_cdc_health()

  Checks CDC health for all stream tables via pgtrickle.check_cdc_health().
  Reports trigger/WAL status, buffer table sizes, and any replication slot issues.
  Raises an error if any source has problems, causing dbt run-operation to exit non-zero.

  Usage:
    dbt run-operation pgtrickle_check_cdc_health
#}
{% macro pgtrickle_check_cdc_health() %}
  {% if execute %}
    {% set query %}
      SELECT * FROM pgtrickle.check_cdc_health()
    {% endset %}
    {% set results = run_query(query) %}
    {% set problems = [] %}
    {% for row in results.rows %}
      {% set st = row['pgt_schema'] ~ '.' ~ row['pgt_name'] %}
      {% set source = row['source_schema'] ~ '.' ~ row['source_table'] %}
      {{ log("CDC: " ~ st ~ " ← " ~ source ~ " [" ~ row['cdc_mode'] ~ "] buffer=" ~ row['buffer_rows'], info=true) }}
      {% if row['healthy'] == false %}
        {% do problems.append(st ~ " ← " ~ source ~ ": " ~ row['issue']) %}
      {% endif %}
    {% endfor %}
    {% if problems | length > 0 %}
      {{ exceptions.raise_compiler_error(
           "CDC health check failed:\n" ~ problems | join("\n")
         ) }}
    {% endif %}
  {% endif %}
{% endmacro %}
