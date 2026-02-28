{#
  pgtrickle_check_freshness(model_name, warn_seconds, error_seconds)

  Checks freshness of stream tables via pg_trickle's pg_stat_stream_tables view.
  Raises a compiler error if any stream table exceeds the error threshold,
  causing `dbt run-operation` to exit non-zero (essential for CI).

  Usage:
    dbt run-operation pgtrickle_check_freshness
    dbt run-operation pgtrickle_check_freshness --args '{model_name: order_totals, warn_seconds: 300, error_seconds: 900}'

  Args:
    model_name (str|none): Specific stream table to check, or all if none
    warn_seconds (int): Staleness threshold for warnings (default: 600 = 10 min)
    error_seconds (int): Staleness threshold for errors (default: 1800 = 30 min)
#}
{% macro pgtrickle_check_freshness(model_name=none, warn_seconds=600, error_seconds=1800) %}
  {% if execute %}
    {% set query %}
      SELECT
        pgt_name,
        pgt_schema,
        last_refresh_at,
        EXTRACT(EPOCH FROM staleness)::int AS staleness_seconds,
        stale,
        consecutive_errors
      FROM pgtrickle.pg_stat_stream_tables
      WHERE status = 'ACTIVE'
      {% if model_name is not none %}
        AND pgt_name = {{ dbt.string_literal(model_name) }}
      {% endif %}
    {% endset %}
    {% set results = run_query(query) %}
    {% set errors = [] %}
    {% for row in results.rows %}
      {% set name = row['pgt_schema'] ~ '.' ~ row['pgt_name'] %}
      {% set staleness = row['staleness_seconds'] %}
      {% if staleness is not none and staleness > error_seconds %}
        {{ log("ERROR: stream table '" ~ name ~ "' is stale (" ~ staleness ~ "s > " ~ error_seconds ~ "s)", info=true) }}
        {% do errors.append(name) %}
      {% elif staleness is not none and staleness > warn_seconds %}
        {{ log("WARN: stream table '" ~ name ~ "' is approaching staleness (" ~ staleness ~ "s > " ~ warn_seconds ~ "s)", info=true) }}
      {% else %}
        {{ log("OK: stream table '" ~ name ~ "' is fresh (" ~ staleness ~ "s)", info=true) }}
      {% endif %}
      {% if row['consecutive_errors'] > 0 %}
        {{ log("WARN: stream table '" ~ name ~ "' has " ~ row['consecutive_errors'] ~ " consecutive error(s)", info=true) }}
      {% endif %}
    {% endfor %}
    {% if errors | length > 0 %}
      {{ exceptions.raise_compiler_error(
           "Freshness check failed: " ~ errors | length ~ " stream table(s) exceeded error threshold ("
           ~ error_seconds ~ "s): " ~ errors | join(", ")
         ) }}
    {% endif %}
  {% endif %}
{% endmacro %}
