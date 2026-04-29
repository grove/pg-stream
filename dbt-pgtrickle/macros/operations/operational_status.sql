{#
  pgtrickle_operational_status()

  Returns the current pg_trickle operational state as a dict, including:
    - scheduler_running: whether the scheduler BGW is active
    - is_drained: whether the scheduler is in drain mode
    - cdc_paused: whether CDC capture is paused (changes are discarded)
    - force_full: whether all stream tables are forced to FULL refresh
    - backpressure_active: whether WAL backpressure is enforced
    - enabled: whether pg_trickle is enabled cluster-wide

  Added in v0.40.0 (O40-5) to expose the operational surface added in
  v0.35–v0.39 to dbt users.

  Usage:
    {% set state = dbt_pgtrickle.pgtrickle_operational_status() %}
    {% if state.is_drained %}
      {{ exceptions.raise_compiler_error("pg_trickle scheduler is drained — aborting.") }}
    {% endif %}
    {% if state.cdc_paused %}
      {{ log("WARNING: CDC is paused; stream tables may not reflect recent changes.", info=true) }}
    {% endif %}
#}
{% macro pgtrickle_operational_status() %}
  {% if execute %}
    {% set query %}
      SELECT
        (SELECT count(*) > 0 FROM pg_stat_activity
         WHERE backend_type = 'pg_trickle scheduler') AS scheduler_running,
        pgtrickle.is_drained()                           AS is_drained,
        current_setting('pg_trickle.cdc_paused', true) = 'on'      AS cdc_paused,
        current_setting('pg_trickle.force_full_refresh', true) = 'on' AS force_full,
        current_setting('pg_trickle.enforce_backpressure', true) = 'on' AS backpressure_active,
        current_setting('pg_trickle.enabled', true) = 'on'         AS enabled
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {% set row = result.rows[0] %}
      {{ return({
        'scheduler_running': row['scheduler_running'],
        'is_drained': row['is_drained'],
        'cdc_paused': row['cdc_paused'],
        'force_full': row['force_full'],
        'backpressure_active': row['backpressure_active'],
        'enabled': row['enabled']
      }) }}
    {% endif %}
  {% endif %}
  {{ return({
    'scheduler_running': false,
    'is_drained': true,
    'cdc_paused': false,
    'force_full': false,
    'backpressure_active': false,
    'enabled': false
  }) }}
{% endmacro %}

{#
  pgtrickle_drain(timeout_s)

  Drains the pg_trickle scheduler via pgtrickle.drain().
  Blocks until all in-flight refresh workers complete or the timeout expires.

  Returns true if drain completed, false if timed out.

  Added in v0.40.0 (O40-5).

  Usage:
    {% set drained = dbt_pgtrickle.pgtrickle_drain(60) %}
    {% if not drained %}
      {{ exceptions.raise_compiler_error("pg_trickle drain timed out.") }}
    {% endif %}
#}
{% macro pgtrickle_drain(timeout_s=60) %}
  {% if execute %}
    {% set query %}
      SELECT pgtrickle.drain(timeout => {{ timeout_s }})
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {{ return(result.rows[0][0]) }}
    {% endif %}
  {% endif %}
  {{ return(false) }}
{% endmacro %}
