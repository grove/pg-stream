{#
  refresh_all_stream_tables(schema)

  Refreshes all active dbt-managed stream tables in dependency order.
  Queries the pg_trickle dependency catalog to determine the correct
  topological ordering so upstream tables are refreshed before downstream ones.

  Designed for CI pipelines: run after `dbt run` and before `dbt test` to
  ensure all materialized data is current.

  Usage:
    dbt run-operation refresh_all_stream_tables
    dbt run-operation refresh_all_stream_tables --args '{"schema": "analytics"}'

  Args:
    schema (str|none): Only refresh stream tables in this schema.
                       Defaults to all dbt-managed stream tables.
#}
{% macro refresh_all_stream_tables(schema=none) %}
  {% if execute %}
    {# Collect dbt-managed stream table models from the graph #}
    {% set models = graph.nodes.values()
         | selectattr('config.materialized', 'equalto', 'stream_table') %}
    {% set dbt_stream_tables = {} %}
    {% for model in models %}
      {% set st_name = model.config.get('stream_table_name', model.name) %}
      {% set st_schema = model.config.get('stream_table_schema', target.schema) %}
      {% if schema is none or st_schema == schema %}
        {% do dbt_stream_tables.update({st_schema ~ '.' ~ st_name: true}) %}
      {% endif %}
    {% endfor %}

    {% if dbt_stream_tables | length == 0 %}
      {{ log("pg_trickle: no dbt-managed stream tables found to refresh", info=true) }}
      {{ return('') }}
    {% endif %}

    {# Query the pg_trickle dependency catalog for topological ordering.
       We use a recursive CTE to compute depth (distance from root/source tables)
       so we can refresh in correct dependency order (depth 0 first, then 1, etc.) #}
    {% set order_query %}
      WITH RECURSIVE dep_depth AS (
        -- Base: stream tables with no stream-table dependencies (leaf-level)
        SELECT
          st.pgt_id,
          st.pgt_schema,
          st.pgt_name,
          0 AS depth
        FROM pgtrickle.pgt_stream_tables st
        WHERE st.status = 'ACTIVE'
          AND NOT EXISTS (
            SELECT 1
            FROM pgtrickle.pgt_dependencies d
            JOIN pgtrickle.pgt_stream_tables upstream
              ON d.source_relid = (upstream.pgt_schema || '.' || upstream.pgt_name)::regclass
            WHERE d.pgt_id = st.pgt_id
              AND d.source_type = 'STREAM_TABLE'
          )

        UNION ALL

        -- Recursive: stream tables depending on already-computed ones
        SELECT
          st.pgt_id,
          st.pgt_schema,
          st.pgt_name,
          dd.depth + 1
        FROM pgtrickle.pgt_stream_tables st
        JOIN pgtrickle.pgt_dependencies d ON d.pgt_id = st.pgt_id
        JOIN dep_depth dd ON d.source_relid = (dd.pgt_schema || '.' || dd.pgt_name)::regclass
        WHERE st.status = 'ACTIVE'
          AND d.source_type = 'STREAM_TABLE'
      )
      SELECT pgt_schema, pgt_name, MAX(depth) AS depth
      FROM dep_depth
      GROUP BY pgt_id, pgt_schema, pgt_name
      ORDER BY depth ASC, pgt_name ASC
    {% endset %}

    {% set results = run_query(order_query) %}
    {% set refreshed = [] %}
    {% set skipped = [] %}

    {% for row in results.rows %}
      {% set qualified = row['pgt_schema'] ~ '.' ~ row['pgt_name'] %}
      {% if qualified in dbt_stream_tables %}
        {{ log("pg_trickle: refreshing '" ~ qualified ~ "' (depth " ~ row['depth'] ~ ")", info=true) }}
        {{ dbt_pgtrickle.pgtrickle_refresh_stream_table(qualified) }}
        {% do refreshed.append(qualified) %}
      {% else %}
        {% do skipped.append(qualified) %}
      {% endif %}
    {% endfor %}

    {# Handle dbt-managed stream tables not found in the dependency query
       (e.g., freshly created, not yet in dependency catalog) — refresh them last #}
    {% for qualified in dbt_stream_tables %}
      {% if qualified not in refreshed and qualified not in skipped %}
        {% if dbt_pgtrickle.pgtrickle_stream_table_exists(qualified) %}
          {{ log("pg_trickle: refreshing '" ~ qualified ~ "' (no dependency info)", info=true) }}
          {{ dbt_pgtrickle.pgtrickle_refresh_stream_table(qualified) }}
          {% do refreshed.append(qualified) %}
        {% else %}
          {{ log("pg_trickle: skipping '" ~ qualified ~ "' (not found in catalog)", info=true) }}
        {% endif %}
      {% endif %}
    {% endfor %}

    {{ log("pg_trickle: refreshed " ~ refreshed | length ~ " stream table(s) in dependency order", info=true) }}
  {% endif %}
{% endmacro %}
