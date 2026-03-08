{#
  stream_table materialization

  Custom dbt materialization that maps dbt's lifecycle onto pg_trickle's SQL API:
    - First run:      create_stream_table()
    - Subsequent run: alter_stream_table() if config changed; drop/recreate if query changed
    - Full refresh:   drop_stream_table() + create_stream_table()

  Config keys:
    materialized: 'stream_table'
    schedule: str|null (default '1m')
    refresh_mode: 'AUTO', 'FULL', 'DIFFERENTIAL', or 'IMMEDIATE' (default 'AUTO')
    initialize: bool (default true)
    status: 'ACTIVE' or 'PAUSED' or null (default null — no change)
    stream_table_name: str (default model name)
    stream_table_schema: str (default target schema)
#}
{% materialization stream_table, adapter='postgres' %}

  {%- set target_relation = this.incorporate(type='table') -%}

  {# -- Model config -- #}
  {%- set schedule = config.get('schedule', '1m') -%}
  {%- set refresh_mode = config.get('refresh_mode', 'AUTO') -%}
  {%- set cdc_mode = config.get('cdc_mode', none) -%}
  {%- set initialize = config.get('initialize', true) -%}
  {%- set status = config.get('status', none) -%}
  {%- set st_name = config.get('stream_table_name', target_relation.identifier) -%}
  {%- set st_schema = config.get('stream_table_schema', target_relation.schema) -%}
  {#- should_full_refresh() is the stable API from dbt 1.0+; flags.FULL_REFRESH
      was deprecated in dbt 1.10 and may warn or fail in 1.11+. -#}
  {%- set full_refresh_mode = should_full_refresh() -%}

  {# -- Always schema-qualify the stream table name -- #}
  {%- set qualified_name = st_schema ~ '.' ~ st_name -%}

  {# -- Authoritative existence check via pg_trickle catalog.
       We don't rely solely on dbt's relation cache because the stream table
       may have been created/dropped outside dbt. -- #}
  {%- set st_exists = dbt_pgtrickle.pgtrickle_stream_table_exists(qualified_name) -%}

  {{ log("pg_trickle: materializing stream table '" ~ qualified_name ~ "'", info=true) }}

  {{ run_hooks(pre_hooks) }}

  {# -- Full refresh: drop and recreate -- #}
  {% if full_refresh_mode and st_exists %}
    {{ dbt_pgtrickle.pgtrickle_drop_stream_table(qualified_name) }}
    {% set st_exists = false %}
  {% endif %}

  {# -- Get the compiled SQL (the defining query) -- #}
  {%- set defining_query = sql -%}

  {% if not st_exists %}
    {# -- CREATE: stream table does not exist yet -- #}
    {{ dbt_pgtrickle.pgtrickle_create_stream_table(
          qualified_name, defining_query, schedule, refresh_mode, initialize, cdc_mode
       ) }}
  {% else %}
    {# -- UPDATE: stream table exists — check if query changed -- #}
    {%- set current_info = dbt_pgtrickle.pgtrickle_get_stream_table_info(qualified_name) -%}

    {% if current_info and current_info.defining_query != defining_query %}
      {# Query changed: use ALTER ... query => to migrate in place #}
      {{ log("pg_trickle: query changed — altering '" ~ qualified_name ~ "' in place", info=true) }}
      {{ dbt_pgtrickle.pgtrickle_alter_stream_table(
           qualified_name, schedule, refresh_mode,
         status=status, current_info=current_info,
         cdc_mode=cdc_mode,
           query=defining_query
         ) }}
    {% else %}
      {# Query unchanged: update schedule/mode/status if they differ.
         Pass current_info to avoid redundant catalog lookup. #}
      {{ dbt_pgtrickle.pgtrickle_alter_stream_table(
           qualified_name, schedule, refresh_mode,
         status=status, current_info=current_info,
         cdc_mode=cdc_mode
         ) }}
    {% endif %}
  {% endif %}

  {# dbt requires the 'main' statement to be executed at least once.
     Our DDL runs via run_query() (separate connection), so we satisfy the
     framework with a lightweight no-op on the main connection. #}
  {% call statement('main') %}
    SELECT 1
  {% endcall %}

  {{ run_hooks(post_hooks) }}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
