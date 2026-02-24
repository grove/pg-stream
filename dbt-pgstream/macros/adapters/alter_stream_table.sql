{#
  pgstream_alter_stream_table(name, schedule, refresh_mode, status, current_info)

  Alters an existing stream table via pgstream.alter_stream_table().
  Only sends changes — parameters that match the current state are passed as NULL
  (which pg_stream treats as "keep current value").

  Args:
    name (str): Stream table name (schema-qualified)
    schedule (str|none): New schedule, or none to keep current
    refresh_mode (str|none): New refresh mode, or none to keep current
    status (str|none): 'ACTIVE' or 'PAUSED', or none to keep current
    current_info (dict|none): Pre-fetched metadata from get_stream_table_info().
                              If provided, avoids a redundant catalog lookup.
#}
{% macro pgstream_alter_stream_table(name, schedule, refresh_mode, status=none, current_info=none) %}
  {# Use pre-fetched metadata if available, otherwise look it up #}
  {% set current = current_info if current_info else pgstream_get_stream_table_info(name) %}
  {% if current %}
    {% set needs_alter = false %}

    {% if current.schedule != schedule %}
      {% set needs_alter = true %}
    {% endif %}

    {% if current.refresh_mode != refresh_mode %}
      {% set needs_alter = true %}
    {% endif %}

    {% if status is not none and current.status != status %}
      {% set needs_alter = true %}
    {% endif %}

    {% if needs_alter %}
      {% set alter_sql %}
        SELECT pgstream.alter_stream_table(
          {{ dbt.string_literal(name) }},
          schedule => {% if current.schedule != schedule %}{% if schedule is none %}NULL{% else %}{{ dbt.string_literal(schedule) }}{% endif %}{% else %}NULL{% endif %},
          refresh_mode => {% if current.refresh_mode != refresh_mode %}{% if refresh_mode is none %}NULL{% else %}{{ dbt.string_literal(refresh_mode) }}{% endif %}{% else %}NULL{% endif %},
          status => {% if status is not none and current.status != status %}{{ dbt.string_literal(status) }}{% else %}NULL{% endif %}
        )
      {% endset %}
      {% do run_query(alter_sql) %}
      {{ log("pg_stream: altered stream table '" ~ name ~ "'", info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
