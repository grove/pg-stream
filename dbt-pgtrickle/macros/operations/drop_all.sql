{#
  drop_all_stream_tables()

  Drops only dbt-managed stream tables (those with materialized: stream_table
  in the dbt graph). Safe in shared environments.

  Usage:
    dbt run-operation drop_all_stream_tables

  drop_all_stream_tables_force()

  Drops ALL stream tables from the pg_trickle catalog, including those created
  outside dbt. Use with caution.

  Usage:
    dbt run-operation drop_all_stream_tables_force
#}

{% macro drop_all_stream_tables() %}
  {% if execute %}
    {% set dropped = [] %}
    {% set models = graph.nodes.values()
         | selectattr('config.materialized', 'equalto', 'stream_table') %}
    {% for model in models %}
      {% set st_name = model.config.get('stream_table_name', model.name) %}
      {% set st_schema = model.config.get('stream_table_schema', target.schema) %}
      {% set qualified = st_schema ~ '.' ~ st_name %}
      {% if pgtrickle_stream_table_exists(qualified) %}
        {{ pgtrickle_drop_stream_table(qualified) }}
        {% do dropped.append(qualified) %}
      {% endif %}
    {% endfor %}
    {{ log("pg_trickle: dropped " ~ dropped | length ~ " dbt-managed stream table(s)", info=true) }}
  {% endif %}
{% endmacro %}


{% macro drop_all_stream_tables_force() %}
  {% if execute %}
    {% set query %}
      SELECT pgt_schema || '.' || pgt_name AS qualified_name
      FROM pgtrickle.pgt_stream_tables
    {% endset %}
    {% set results = run_query(query) %}
    {% if results and results.rows | length > 0 %}
      {% for row in results.rows %}
        {{ pgtrickle_drop_stream_table(row['qualified_name']) }}
      {% endfor %}
      {{ log("pg_trickle: force-dropped " ~ results.rows | length ~ " stream table(s)", info=true) }}
    {% else %}
      {{ log("pg_trickle: no stream tables found to drop", info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
