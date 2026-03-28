{#
  pgtrickle_alter_stream_table(name, schedule, refresh_mode, status, current_info, query, cdc_mode, fuse, fuse_ceiling, fuse_sensitivity)

  Alters an existing stream table via pgtrickle.alter_stream_table().
  Only sends changes — parameters that match the current state are passed as NULL
  (which pg_trickle treats as "keep current value").

  Args:
    name (str): Stream table name (schema-qualified)
    schedule (str|none): New schedule, or none to keep current
    refresh_mode (str|none): New refresh mode, or none to keep current
    status (str|none): 'ACTIVE' or 'PAUSED', or none to keep current
    current_info (dict|none): Pre-fetched metadata from get_stream_table_info().
                              If provided, avoids a redundant catalog lookup.
    query (str|none): New defining query, or none to keep current
    cdc_mode (str|none): New CDC mode, or none to keep current
    fuse (str|none): New fuse mode ('off', 'on', 'auto'), or none to keep current
    fuse_ceiling (int|none): New fuse change-count ceiling, or none to keep current
    fuse_sensitivity (int|none): New fuse sensitivity (consecutive observations), or none to keep current
#}
{% macro pgtrickle_alter_stream_table(name, schedule, refresh_mode, status=none, current_info=none, query=none, cdc_mode=none, fuse=none, fuse_ceiling=none, fuse_sensitivity=none) %}
  {# Use pre-fetched metadata if available, otherwise look it up #}
  {% set current = current_info if current_info else dbt_pgtrickle.pgtrickle_get_stream_table_info(name) %}
  {% if current %}
    {% set needs_alter = false %}

    {% if query is not none and current.defining_query != query %}
      {% set needs_alter = true %}
    {% endif %}

    {% if current.schedule != schedule %}
      {% set needs_alter = true %}
    {% endif %}

    {% if current.refresh_mode != refresh_mode %}
      {% set needs_alter = true %}
    {% endif %}

    {% if current.requested_cdc_mode != cdc_mode %}
      {% set needs_alter = true %}
    {% endif %}

    {% if status is not none and current.status != status %}
      {% set needs_alter = true %}
    {% endif %}

    {# Fuse change detection: only alter when the dbt config explicitly sets a fuse param
       and it differs from the current catalog value. #}
    {% if fuse is not none and current.fuse_mode != fuse %}
      {% set needs_alter = true %}
    {% endif %}
    {% if fuse_ceiling is not none and current.fuse_ceiling | string != fuse_ceiling | string %}
      {% set needs_alter = true %}
    {% endif %}
    {% if fuse_sensitivity is not none and current.fuse_sensitivity | string != fuse_sensitivity | string %}
      {% set needs_alter = true %}
    {% endif %}

    {% if needs_alter %}
      {% call statement('pgtrickle_alter', auto_begin=False, fetch_result=False) %}
        BEGIN;
        SELECT pgtrickle.alter_stream_table(
          {{ dbt.string_literal(name) }},
          query => {% if query is not none and current.defining_query != query %}$pgtrickle${{ query }}$pgtrickle${% else %}NULL{% endif %},
          schedule => {% if current.schedule != schedule %}{% if schedule is none %}NULL{% else %}{{ dbt.string_literal(schedule) }}{% endif %}{% else %}NULL{% endif %},
          refresh_mode => {% if current.refresh_mode != refresh_mode %}{% if refresh_mode is none %}NULL{% else %}{{ dbt.string_literal(refresh_mode) }}{% endif %}{% else %}NULL{% endif %},
          status => {% if status is not none and current.status != status %}{{ dbt.string_literal(status) }}{% else %}NULL{% endif %},
          cdc_mode => {% if current.requested_cdc_mode != cdc_mode %}{% if cdc_mode is none %}NULL{% else %}{{ dbt.string_literal(cdc_mode) }}{% endif %}{% else %}NULL{% endif %},
          fuse => {% if fuse is not none and current.fuse_mode != fuse %}{{ dbt.string_literal(fuse) }}{% else %}NULL{% endif %},
          fuse_ceiling => {% if fuse_ceiling is not none and current.fuse_ceiling | string != fuse_ceiling | string %}{{ fuse_ceiling }}{% else %}NULL{% endif %},
          fuse_sensitivity => {% if fuse_sensitivity is not none and current.fuse_sensitivity | string != fuse_sensitivity | string %}{{ fuse_sensitivity }}{% else %}NULL{% endif %}
        );
        COMMIT;
      {% endcall %}
      {{ log("pg_trickle: altered stream table '" ~ name ~ "'", info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
