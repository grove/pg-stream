{#
  pgtrickle_has_create_or_replace()

  Detects whether the installed pg_trickle version supports
  create_or_replace_stream_table() (≥ 0.6.0).

  Returns true if the function exists, false otherwise.
  Result is cached in the dbt run context.
#}
{% macro pgtrickle_has_create_or_replace() %}
  {# Cache key to avoid probing on every model invocation #}
  {%- set cache_key = '__pgtrickle_has_cor' -%}
  {%- if execute -%}
    {%- if cache_key not in context -%}
      {%- set query -%}
        SELECT EXISTS(
          SELECT 1 FROM pg_proc p
          JOIN pg_namespace n ON n.oid = p.pronamespace
          WHERE n.nspname = 'pgtrickle'
            AND p.proname = 'create_or_replace_stream_table'
        )
      {%- endset -%}
      {%- set result = run_query(query) -%}
      {%- if result and result.rows | length > 0 -%}
        {%- do context.update({cache_key: result.rows[0][0]}) -%}
      {%- else -%}
        {%- do context.update({cache_key: false}) -%}
      {%- endif -%}
    {%- endif -%}
    {{ return(context[cache_key]) }}
  {%- else -%}
    {{ return(false) }}
  {%- endif -%}
{% endmacro %}
