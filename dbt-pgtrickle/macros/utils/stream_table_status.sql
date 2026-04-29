{#
  pgtrickle_stream_table_status(name, warn_seconds, error_seconds)

  Returns the health status of a stream table as a dict with:
    - status: 'healthy', 'stale', 'erroring', 'paused', 'drained', or 'not_found'
    - staleness_seconds: seconds since last refresh (null if never refreshed)
    - consecutive_errors: number of consecutive refresh failures
    - last_refresh_at: timestamp of last refresh
    - total_refreshes: lifetime refresh count
    - is_populated: whether the stream table has been populated
    - refresh_mode: 'FULL', 'DIFFERENTIAL', or 'INCREMENTAL'
    - cdc_paused: whether CDC capture is paused for this stream table
    - force_full: whether force-full override is active cluster-wide

  Added in v0.40.0 (O40-5): exposes cdc_paused, force_full, and drained status.

  Designed for dbt tests — return value can be checked in assertions:

    {% set st = dbt_pgtrickle.pgtrickle_stream_table_status('order_totals') %}
    {% if st.status != 'healthy' %}
      {{ exceptions.raise_compiler_error("Stream table is " ~ st.status) }}
    {% endif %}

  Args:
    name (str): Stream table name. May be schema-qualified ('analytics.order_totals')
                or unqualified ('order_totals' — defaults to target.schema).
    warn_seconds (int): Staleness threshold for 'stale' status (default: 300 = 5 min)
#}
{% macro pgtrickle_stream_table_status(name, warn_seconds=300) %}
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
      SELECT
        s.status AS st_status,
        EXTRACT(EPOCH FROM s.staleness)::int AS staleness_seconds,
        s.consecutive_errors,
        s.last_refresh_at,
        s.total_refreshes,
        s.is_populated,
        s.stale,
        s.refresh_mode,
        current_setting('pg_trickle.cdc_paused', true) = 'on'        AS cdc_paused,
        current_setting('pg_trickle.force_full_refresh', true) = 'on' AS force_full,
        pgtrickle.is_drained()                                         AS is_drained
      FROM pgtrickle.pg_stat_stream_tables s
      WHERE s.pgt_schema = {{ dbt.string_literal(lookup_schema) }}
        AND s.pgt_name = {{ dbt.string_literal(lookup_name) }}
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {% set row = result.rows[0] %}

      {# Classify health status — new in v0.40.0: expose drained and cdc_paused #}
      {% if row['is_drained'] == true %}
        {% set health = 'drained' %}
      {% elif row['st_status'] == 'PAUSED' or row['cdc_paused'] == true %}
        {% set health = 'paused' %}
      {% elif row['consecutive_errors'] > 0 %}
        {% set health = 'erroring' %}
      {% elif row['stale'] == true or (row['staleness_seconds'] is not none and row['staleness_seconds'] > warn_seconds) %}
        {% set health = 'stale' %}
      {% else %}
        {% set health = 'healthy' %}
      {% endif %}

      {{ return({
        'status': health,
        'staleness_seconds': row['staleness_seconds'],
        'consecutive_errors': row['consecutive_errors'],
        'last_refresh_at': row['last_refresh_at'],
        'total_refreshes': row['total_refreshes'],
        'is_populated': row['is_populated'],
        'refresh_mode': row['refresh_mode'],
        'cdc_paused': row['cdc_paused'],
        'force_full': row['force_full'],
        'is_drained': row['is_drained']
      }) }}
    {% endif %}
  {% endif %}
  {{ return({
    'status': 'not_found',
    'staleness_seconds': none,
    'consecutive_errors': 0,
    'last_refresh_at': none,
    'total_refreshes': 0,
    'is_populated': false,
    'refresh_mode': none,
    'cdc_paused': false,
    'force_full': false,
    'is_drained': false
  }) }}
{% endmacro %}
