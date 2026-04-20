{# DBT-1: Enable pg_trickle self-monitoring monitoring.
   Calls setup_self_monitoring() — idempotent, safe on every run.
   Add to dbt_project.yml: +post-hook: "{{ pgtrickle_enable_monitoring() }}" #}
{% macro pgtrickle_enable_monitoring() %}
    {% set sql %}
        SELECT pgtrickle.setup_self_monitoring();
    {% endset %}
    {% do run_query(sql) %}
    {{ log("pg_trickle self-monitoring monitoring enabled (5 DF stream tables active).", info=True) }}
{% endmacro %}
