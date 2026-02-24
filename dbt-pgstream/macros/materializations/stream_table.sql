{#
  stream_table materialization

  Custom dbt materialization that maps dbt's lifecycle onto pg_stream's SQL API:
    - First run:      create_stream_table()
    - Subsequent run: alter_stream_table() if config changed; drop/recreate if query changed
    - Full refresh:   drop_stream_table() + create_stream_table()

  Config keys:
    materialized: 'stream_table'
    schedule: str|null (default '1m')
    refresh_mode: 'FULL' or 'DIFFERENTIAL' (default 'DIFFERENTIAL')
    initialize: bool (default true)
    status: 'ACTIVE' or 'PAUSED' or null (default null — no change)
    stream_table_name: str (default model name)
    stream_table_schema: str (default target schema)
#}
{% materialization stream_table, adapter='postgres' %}

  {%- set target_relation = this.incorporate(type='table') -%}

  {# -- Model config -- #}
  {%- set schedule = config.get('schedule', '1m') -%}
  {%- set refresh_mode = config.get('refresh_mode', 'DIFFERENTIAL') -%}
  {%- set initialize = config.get('initialize', true) -%}
  {%- set status = config.get('status', none) -%}
  {%- set st_name = config.get('stream_table_name', target_relation.identifier) -%}
  {%- set st_schema = config.get('stream_table_schema', target_relation.schema) -%}
  {%- set full_refresh_mode = (flags.FULL_REFRESH == True) -%}

  {# -- Always schema-qualify the stream table name -- #}
  {%- set qualified_name = st_schema ~ '.' ~ st_name -%}

  {# -- Authoritative existence check via pg_stream catalog.
       We don't rely solely on dbt's relation cache because the stream table
       may have been created/dropped outside dbt. -- #}
  {%- set st_exists = pgstream_stream_table_exists(qualified_name) -%}

  {{ log("pg_stream: materializing stream table '" ~ qualified_name ~ "'", info=true) }}

  {{ run_hooks(pre_hooks) }}

  {# -- Full refresh: drop and recreate -- #}
  {% if full_refresh_mode and st_exists %}
    {{ pgstream_drop_stream_table(qualified_name) }}
    {% set st_exists = false %}
  {% endif %}

  {# -- Get the compiled SQL (the defining query) -- #}
  {%- set defining_query = sql -%}

  {% if not st_exists %}
    {# -- CREATE: stream table does not exist yet -- #}
    {{ pgstream_create_stream_table(
         qualified_name, defining_query, schedule, refresh_mode, initialize
       ) }}
    {% do adapter.cache_new(this.incorporate(type='table')) %}
  {% else %}
    {# -- UPDATE: stream table exists — check if query changed -- #}
    {%- set current_info = pgstream_get_stream_table_info(qualified_name) -%}

    {% if current_info and current_info.defining_query != defining_query %}
      {# Query changed: must drop and recreate (no in-place ALTER for query) #}
      {{ log("pg_stream: query changed — dropping and recreating '" ~ qualified_name ~ "'", info=true) }}
      {{ pgstream_drop_stream_table(qualified_name) }}
      {{ pgstream_create_stream_table(
           qualified_name, defining_query, schedule, refresh_mode, initialize
         ) }}
    {% else %}
      {# Query unchanged: update schedule/mode/status if they differ.
         Pass current_info to avoid redundant catalog lookup. #}
      {{ pgstream_alter_stream_table(
           qualified_name, schedule, refresh_mode,
           status=status, current_info=current_info
         ) }}
    {% endif %}
  {% endif %}

  {{ run_hooks(post_hooks) }}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
