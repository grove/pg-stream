# Plan: dbt Integration via Full Custom Adapter

**Option B — Dedicated `dbt-pgtrickle` Adapter**

Date: 2026-02-24
Status: PROPOSED

> **Note (2026-02-24):** Option A — the macro-only package
> ([PLAN_DBT_MACRO.md](PLAN_DBT_MACRO.md)) — has been **implemented** in
> `dbt-pgtrickle/` (Phases 1–8, 10). CI pipeline (Phase 9) is live in
> `.github/workflows/ci.yml`. This adapter plan (Option B) is the *upgrade
> path* for when first-class relation types, column filtering, and native
> source-freshness support are needed.

---

## Overview

Implement pg_trickle integration with [dbt Core](https://docs.getdbt.com/docs/introduction)
as a **full custom adapter** (`dbt-pgtrickle`) that extends `dbt-postgres`. This approach
gives complete control over how dbt interacts with pg_trickle: custom relation types,
native catalog introspection, column filtering (`__pgt_row_id` hidden), and first-class
support for stream-table-specific operations (manual refresh, CDC health checks, staleness
monitoring).

Option A (macro-only package) is already implemented and provides the core
`stream_table` materialization, SQL API wrappers, lifecycle operations, freshness
monitoring, and integration tests. This adapter plan (Option B) builds on that
foundation when pg_trickle becomes a central part of the data platform and users
need a richer dbt experience — hidden `__pgt_row_id` columns, `stream_table`
relation type in `dbt docs`, and native `dbt source freshness` support.

---

## Table of Contents

- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Phase 1 — Adapter Scaffolding](#phase-1--adapter-scaffolding)
- [Phase 2 — Connection & Credentials](#phase-2--connection--credentials)
- [Phase 3 — Relation Types & Catalog](#phase-3--relation-types--catalog)
- [Phase 4 — Custom Materialization](#phase-4--custom-materialization)
- [Phase 5 — Column Introspection](#phase-5--column-introspection)
- [Phase 6 — Source Freshness](#phase-6--source-freshness)
- [Phase 7 — Custom Operations](#phase-7--custom-operations)
- [Phase 8 — Monitoring Integration](#phase-8--monitoring-integration)
- [Phase 9 — Testing](#phase-9--testing)
- [Phase 10 — Packaging & Distribution](#phase-10--packaging--distribution)
- [Comparison with Option A](#comparison-with-option-a)
- [File Layout](#file-layout)
- [Appendix: Adapter API Surface](#appendix-adapter-api-surface)

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                          dbt Core (CLI)                              │
│                                                                      │
│  Adapter plugin system: discovers dbt-pgtrickle via entry_points      │
│                                                                      │
│  dbt run ──────► PgTrickleAdapter.execute_model()                     │
│  dbt test ─────► Standard test runner (heap table)                   │
│  dbt source freshness ──► PgTrickleAdapter.calculate_freshness()      │
│  dbt docs generate ──────► PgTrickleAdapter.get_columns_in_relation() │
└──────────────────────┬───────────────────────────────────────────────┘
                       │  Python adapter (extends dbt-postgres)
                       ▼
┌──────────────────────────────────────────────────────────────────────┐
│                    PgTrickleAdapter (Python)                           │
│                                                                      │
│  class PgTrickleAdapter(PostgresAdapter):                             │
│    - Overrides: get_columns_in_relation() → hides __pgt_row_id      │
│    - Overrides: list_relations_without_caching() → includes STs     │
│    - New: create_stream_table(), alter_stream_table(), etc.          │
│    - New: get_cdc_health(), get_stream_table_stats()                 │
│                                                                      │
│  class PgTrickleRelation(PostgresRelation):                           │
│    - type: 'stream_table' | 'table' | 'view' | ...                  │
│                                                                      │
│  class PgTrickleColumn(PostgresColumn):                               │
│    - Filters __pgt_row_id from introspection results                 │
└──────────────────────┬───────────────────────────────────────────────┘
                       │  psycopg2 / asyncpg
                       ▼
┌──────────────────────────────────────────────────────────────────────┐
│                       PostgreSQL 18                                   │
│                                                                      │
│  pgtrickle.create_stream_table()    pgtrickle.pgt_stream_tables        │
│  pgtrickle.alter_stream_table()     pgtrickle.pgt_dependencies         │
│  pgtrickle.drop_stream_table()      pgtrickle.pg_stat_stream_tables    │
│  pgtrickle.refresh_stream_table()   pgtrickle.check_cdc_health()       │
│  pgtrickle.explain_st()             pgtrickle.get_refresh_history()    │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Prerequisites

- dbt Core ≥ 1.7
- Python ≥ 3.9
- `dbt-postgres` ≥ 1.7 (base adapter dependency)
- PostgreSQL 18 with pg_trickle extension installed
- `psycopg2-binary` or `psycopg2` (inherited from dbt-postgres)

---

## Phase 1 — Adapter Scaffolding

### 1.1 Python package structure

```
dbt-pgtrickle/
├── pyproject.toml
├── setup.py                         # Or just pyproject.toml with setuptools
├── README.md
├── LICENSE
├── dbt/
│   ├── __init__.py
│   └── adapters/
│       ├── __init__.py
│       └── pgtrickle/
│           ├── __init__.py
│           ├── connections.py       # Connection manager
│           ├── impl.py             # PgTrickleAdapter class
│           ├── relation.py         # PgTrickleRelation class
│           ├── column.py           # PgTrickleColumn class
│           └── pgtrickle_credentials.py
├── dbt/
│   └── include/
│       └── pgtrickle/
│           ├── dbt_project.yml
│           ├── macros/
│           │   ├── materializations/
│           │   │   └── stream_table.sql
│           │   ├── adapters/
│           │   │   └── pgtrickle_api.sql
│           │   ├── catalog.sql
│           │   └── relations.sql
│           └── profile_template.yml
└── tests/
    ├── conftest.py
    ├── unit/
    │   ├── test_adapter.py
    │   └── test_relation.py
    └── functional/
        ├── test_stream_table_materialization.py
        ├── test_column_filtering.py
        └── test_freshness.py
```

### 1.2 Entry point registration

```toml
# pyproject.toml
[project]
name = "dbt-pgtrickle"
version = "0.1.0"
description = "dbt adapter for pg_trickle (PostgreSQL streaming tables)"
requires-python = ">=3.9"
dependencies = [
    "dbt-core>=1.7,<2.0",
    "dbt-postgres>=1.7,<2.0",
]

[project.entry-points."dbt.adapters"]
pgtrickle = "dbt.adapters.pgtrickle"
```

### 1.3 Profile type

```yaml
# dbt/include/pgtrickle/profile_template.yml
fixed:
  type: pgtrickle
prompts:
  host:
    hint: 'Hostname for the PostgreSQL instance'
    default: 'localhost'
  port:
    hint: 'Port number'
    default: 5432
  user:
    hint: 'Database user'
  pass:
    type: 'password'
    hint: 'Database password'
  dbname:
    hint: 'Database name'
  schema:
    hint: 'Default schema'
    default: 'public'
```

User profile:

```yaml
# ~/.dbt/profiles.yml
my_project:
  target: dev
  outputs:
    dev:
      type: pgtrickle          # ← uses the custom adapter
      host: localhost
      port: 5432
      user: postgres
      password: postgres
      dbname: mydb
      schema: public
      threads: 4
```

---

## Phase 2 — Connection & Credentials

### 2.1 Credentials class

File: `dbt/adapters/pgtrickle/pgtrickle_credentials.py`

```python
from dbt.adapters.postgres import PostgresCredentials
from dataclasses import dataclass


@dataclass
class PgTrickleCredentials(PostgresCredentials):
    """Extends PostgresCredentials with pg_trickle-specific options."""

    @property
    def type(self) -> str:
        return "pgtrickle"
```

No additional credentials fields are needed — pg_trickle uses the same PostgreSQL
connection. If future features require stream-table-specific config (e.g., default
schedule), add them here.

### 2.2 Connection manager

File: `dbt/adapters/pgtrickle/connections.py`

```python
from dbt.adapters.postgres import PostgresConnectionManager
from dbt.adapters.pgtrickle.pgtrickle_credentials import PgTrickleCredentials


class PgTrickleConnectionManager(PostgresConnectionManager):
    TYPE = "pgtrickle"

    @classmethod
    def open(cls, connection):
        """Open a connection and verify pg_trickle extension is available."""
        connection = super().open(connection)
        # Optionally verify extension is installed
        cls._verify_pgtrickle_extension(connection)
        return connection

    @classmethod
    def _verify_pgtrickle_extension(cls, connection):
        """Check that pg_trickle extension exists in the database."""
        cursor = connection.handle.cursor()
        cursor.execute(
            "SELECT 1 FROM pg_extension WHERE extname = 'pg_trickle'"
        )
        if cursor.fetchone() is None:
            raise RuntimeError(
                "pg_trickle extension is not installed. "
                "Run: CREATE EXTENSION pg_trickle;"
            )
```

---

## Phase 3 — Relation Types & Catalog

### 3.1 Custom relation type

File: `dbt/adapters/pgtrickle/relation.py`

```python
from dbt.adapters.postgres.relation import PostgresRelation
from dbt.adapters.contracts.relation import RelationType
from dataclasses import dataclass


class PgTrickleRelationType(RelationType):
    StreamTable = "stream_table"


@dataclass(frozen=True, eq=False, repr=False)
class PgTrickleRelation(PostgresRelation):
    """Extends PostgresRelation to recognize stream tables."""

    @classmethod
    def get_relation_type(cls) -> type:
        return PgTrickleRelationType

    def is_stream_table(self) -> bool:
        return self.type == PgTrickleRelationType.StreamTable
```

### 3.2 Catalog integration

Override `list_relations_without_caching()` to detect stream tables:

```python
# In PgTrickleAdapter (impl.py)

def list_relations_without_caching(self, schema_relation):
    """
    List all relations, marking pg_trickle-managed tables as 'stream_table' type.
    """
    # Get standard PostgreSQL relations
    relations = super().list_relations_without_caching(schema_relation)

    # Query pgtrickle catalog for stream table OIDs
    st_oids = self._get_stream_table_oids(schema_relation.schema)

    # Reclassify matching relations
    result = []
    for rel in relations:
        if rel.type == RelationType.Table and self._oid_of(rel) in st_oids:
            result.append(rel.incorporate(type=PgTrickleRelationType.StreamTable))
        else:
            result.append(rel)
    return result

def _get_stream_table_oids(self, schema: str) -> set:
    """Get OIDs of all stream tables in a schema."""
    sql = f"""
        SELECT pgt_relid
        FROM pgtrickle.pgt_stream_tables
        WHERE pgt_schema = '{schema}'
    """
    _, result = self.execute(sql, fetch=True)
    return {row[0] for row in result}
```

### 3.3 Catalog macro

File: `dbt/include/pgtrickle/macros/catalog.sql`

```sql
{% macro pgtrickle__get_catalog(information_schema, schemas) -%}
  {# Standard postgres catalog query #}
  {{ postgres__get_catalog(information_schema, schemas) }}
{%- endmacro %}
```

The catalog query can be extended to add `stream_table` as a table_type:

```sql
{% macro pgtrickle__get_catalog_relations(information_schema, relations) -%}
  {%- call statement('catalog', fetch_result=True) -%}
    WITH stream_tables AS (
      SELECT pgt_relid::bigint AS relid,
             pgt_name,
             pgt_schema,
             schedule,
             refresh_mode,
             status
      FROM pgtrickle.pgt_stream_tables
    )
    SELECT
      t.table_catalog AS "table_database",
      t.table_schema AS "table_schema",
      t.table_name AS "table_name",
      CASE
        WHEN st.relid IS NOT NULL THEN 'stream_table'
        ELSE t.table_type
      END AS "table_type",
      t.table_comment AS "table_comment",
      c.column_name AS "column_name",
      c.ordinal_position AS "column_index",
      c.data_type AS "column_type",
      c.column_comment AS "column_comment",
      -- pg_trickle metadata
      st.schedule AS "pgtrickle_schedule",
      st.refresh_mode AS "pgtrickle_refresh_mode",
      st.status AS "pgtrickle_status"
    FROM information_schema.tables t
    JOIN information_schema.columns c
      ON t.table_schema = c.table_schema
     AND t.table_name = c.table_name
    LEFT JOIN stream_tables st
      ON t.table_schema = st.pgt_schema
     AND t.table_name = st.pgt_name
    WHERE (t.table_schema, t.table_name) IN (
      {% for relation in relations %}
        ('{{ relation.schema }}', '{{ relation.identifier }}')
        {% if not loop.last %},{% endif %}
      {% endfor %}
    )
    -- Hide __pgt_row_id from catalog
    AND c.column_name != '__pgt_row_id'
    ORDER BY t.table_schema, t.table_name, c.ordinal_position
  {%- endcall -%}
  {{ return(load_result('catalog').table) }}
{%- endmacro %}
```

---

## Phase 4 — Custom Materialization

### 4.1 Materialization macro

File: `dbt/include/pgtrickle/macros/materializations/stream_table.sql`

The materialization is similar to the macro-only version (Option A) but leverages
adapter-level methods for cleaner integration:

```sql
{% materialization stream_table, adapter='pgtrickle' %}

  {%- set target_relation = this.incorporate(type='stream_table') -%}
  {%- set existing_relation = load_cached_relation(this) -%}

  {%- set schedule = config.get('schedule', '1m') -%}
  {%- set refresh_mode = config.get('refresh_mode', 'DIFFERENTIAL') -%}
  {%- set initialize = config.get('initialize', true) -%}
  {%- set st_name = target_relation.identifier -%}
  {%- set st_schema = target_relation.schema -%}
  {%- set qualified_name = st_schema ~ '.' ~ st_name
        if st_schema != 'public'
        else st_name -%}
  {%- set full_refresh_mode = (flags.FULL_REFRESH == True) -%}

  {{ run_hooks(pre_hooks) }}

  {# -- Full refresh: drop and recreate -- #}
  {% if full_refresh_mode and existing_relation is not none %}
    {% do adapter.pgtrickle_drop_stream_table(qualified_name) %}
    {% set existing_relation = none %}
  {% endif %}

  {%- set defining_query = sql -%}

  {% if existing_relation is none %}
    {# -- CREATE -- #}
    {% do adapter.pgtrickle_create_stream_table(
         qualified_name, defining_query, schedule, refresh_mode, initialize
       ) %}
    {% do adapter.cache_new(target_relation) %}

  {% elif existing_relation.is_stream_table() %}
    {# -- UPDATE: compare query, schedule, mode -- #}
    {% set current = adapter.pgtrickle_get_stream_table_info(qualified_name) %}

    {% if current.defining_query != defining_query %}
      {% do adapter.pgtrickle_drop_stream_table(qualified_name) %}
      {% do adapter.pgtrickle_create_stream_table(
           qualified_name, defining_query, schedule, refresh_mode, initialize
         ) %}
    {% else %}
      {% do adapter.pgtrickle_alter_if_changed(
           qualified_name, schedule, refresh_mode, current
         ) %}
    {% endif %}

  {% else %}
    {# -- Relation exists but is a regular table/view — error -- #}
    {{ exceptions.raise_compiler_error(
         "Cannot create stream table '" ~ qualified_name ~
         "': a " ~ existing_relation.type ~ " with that name already exists."
       ) }}
  {% endif %}

  {{ run_hooks(post_hooks) }}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
```

### 4.2 Adapter-level methods

These Python methods on `PgTrickleAdapter` are called from the materialization:

```python
# In PgTrickleAdapter (impl.py)

def pgtrickle_create_stream_table(
    self, name: str, query: str, schedule: str,
    refresh_mode: str, initialize: bool
):
    sql = f"""
        SELECT pgtrickle.create_stream_table(
            {self._quote_string(name)},
            {self._quote_string(query)},
            {self._quote_string(schedule)},
            {self._quote_string(refresh_mode)},
            {initialize}
        )
    """
    self.execute(sql, auto_begin=True)
    self.connections.get_thread_connection().handle.commit()

def pgtrickle_drop_stream_table(self, name: str):
    sql = f"SELECT pgtrickle.drop_stream_table({self._quote_string(name)})"
    self.execute(sql, auto_begin=True)
    self.connections.get_thread_connection().handle.commit()

def pgtrickle_alter_if_changed(
    self, name: str, schedule: str, refresh_mode: str, current: dict
):
    if current['schedule'] != schedule:
        sql = f"""
            SELECT pgtrickle.alter_stream_table(
                {self._quote_string(name)},
                schedule => {self._quote_string(schedule)}
            )
        """
        self.execute(sql, auto_begin=True)

    if current['refresh_mode'] != refresh_mode:
        sql = f"""
            SELECT pgtrickle.alter_stream_table(
                {self._quote_string(name)},
                refresh_mode => {self._quote_string(refresh_mode)}
            )
        """
        self.execute(sql, auto_begin=True)

    self.connections.get_thread_connection().handle.commit()

def pgtrickle_refresh_stream_table(self, name: str):
    sql = f"SELECT pgtrickle.refresh_stream_table({self._quote_string(name)})"
    self.execute(sql, auto_begin=True)
    self.connections.get_thread_connection().handle.commit()

def pgtrickle_get_stream_table_info(self, name: str) -> dict | None:
    sql = f"""
        SELECT pgt_name, defining_query, schedule, refresh_mode, status
        FROM pgtrickle.pgt_stream_tables
        WHERE pgt_name = {self._quote_string(name)}
    """
    _, result = self.execute(sql, fetch=True)
    if result and len(result) > 0:
        row = result[0]
        return {
            'pgt_name': row[0],
            'defining_query': row[1],
            'schedule': row[2],
            'refresh_mode': row[3],
            'status': row[4],
        }
    return None

def _quote_string(self, value: str) -> str:
    """Safely quote a string for SQL interpolation."""
    escaped = value.replace("'", "''")
    return f"'{escaped}'"
```

---

## Phase 5 — Column Introspection

### 5.1 Hiding `__pgt_row_id`

Override `get_columns_in_relation()` to filter out the internal row ID column:

```python
# In PgTrickleAdapter (impl.py)

def get_columns_in_relation(self, relation):
    columns = super().get_columns_in_relation(relation)
    if hasattr(relation, 'is_stream_table') and relation.is_stream_table():
        columns = [c for c in columns if c.name != '__pgt_row_id']
    return columns
```

This ensures:
- `dbt docs generate` does not show `__pgt_row_id`
- `SELECT *` expansion in dbt does not include the internal column
- Schema tests (e.g., `not_null`, `unique`) are not applied to `__pgt_row_id`

### 5.2 Column type mapping

Stream tables use standard PostgreSQL types. No custom type mapping is needed beyond
what `dbt-postgres` provides.

---

## Phase 6 — Source Freshness

### 6.1 Custom freshness calculation

Override `calculate_freshness()` for stream table sources:

```python
# In PgTrickleAdapter (impl.py)

def calculate_freshness(
    self, source, loaded_at_field, filter_
):
    """
    For pg_trickle-managed sources, use the monitoring view
    instead of querying loaded_at_field on the table.
    """
    # Check if this source is a stream table
    st_info = self.pgtrickle_get_stream_table_info(
        f"{source.schema}.{source.identifier}"
        if source.schema != 'public'
        else source.identifier
    )

    if st_info is not None:
        # Use pg_trickle's native staleness tracking
        sql = f"""
            SELECT
                last_refresh_at AS max_loaded_at,
                now() AS snapshotted_at,
                staleness AS max_loaded_at_time_ago_in_s
            FROM pgtrickle.pg_stat_stream_tables
            WHERE pgt_name = {self._quote_string(st_info['pgt_name'])}
        """
        _, result = self.execute(sql, fetch=True)
        if result and len(result) > 0:
            return {
                'max_loaded_at': result[0][0],
                'snapshotted_at': result[0][1],
                'age': result[0][2],
            }

    # Fall back to standard freshness
    return super().calculate_freshness(source, loaded_at_field, filter_)
```

### 6.2 User experience

```yaml
# sources.yml
sources:
  - name: streaming
    schema: public
    freshness:
      warn_after: {count: 10, period: minute}
      error_after: {count: 30, period: minute}
    tables:
      - name: order_totals
        # No loaded_at_field needed — adapter queries pg_trickle monitoring
```

```bash
$ dbt source freshness --select source:streaming.order_totals
Running with dbt=1.7.0
Found 1 source, 1 exposure
Freshness test for source streaming.order_totals:
  max_loaded_at: 2026-02-24 12:34:56+00
  snapshotted_at: 2026-02-24 12:35:02+00
  age: 6.0 seconds
  status: pass
```

---

## Phase 7 — Custom Operations

### 7.1 Manual refresh operation

```sql
-- macros/operations/refresh.sql
{% macro refresh(model_name) %}
  {% do adapter.pgtrickle_refresh_stream_table(model_name) %}
  {{ log("Refreshed stream table: " ~ model_name, info=true) }}
{% endmacro %}
```

### 7.2 Explain operation

```sql
-- macros/operations/explain.sql
{% macro explain(model_name) %}
  {% set query %}
    SELECT property, value FROM pgtrickle.explain_st({{ dbt.string_literal(model_name) }})
  {% endset %}
  {% set result = run_query(query) %}
  {% for row in result.rows %}
    {{ log(row['property'] ~ ": " ~ row['value'], info=true) }}
  {% endfor %}
{% endmacro %}
```

Usage:
```bash
dbt run-operation explain --args '{"model_name": "order_totals"}'
```

### 7.3 CDC health check operation

```sql
-- macros/operations/cdc_health.sql
{% macro cdc_health() %}
  {% set query %}
    SELECT source_table, cdc_mode, slot_name,
           lag_bytes, confirmed_lsn, alert
    FROM pgtrickle.check_cdc_health()
  {% endset %}
  {% set result = run_query(query) %}
  {% for row in result.rows %}
    {{ log(row['source_table'] ~ " | " ~ row['cdc_mode'] ~
           " | lag=" ~ (row['lag_bytes'] or 'n/a') ~
           " | alert=" ~ (row['alert'] or 'none'), info=true) }}
  {% endfor %}
{% endmacro %}
```

Usage:
```bash
dbt run-operation cdc_health
```

### 7.4 Refresh history operation

```sql
-- macros/operations/refresh_history.sql
{% macro refresh_history(model_name, limit=10) %}
  {% set query %}
    SELECT start_time, action, status,
           rows_inserted, rows_deleted,
           EXTRACT(EPOCH FROM (end_time - start_time))::numeric(10,2) AS duration_s,
           error_message
    FROM pgtrickle.get_refresh_history(
      {{ dbt.string_literal(model_name) }}, {{ limit }}
    )
    ORDER BY start_time DESC
  {% endset %}
  {% set result = run_query(query) %}
  {% for row in result.rows %}
    {{ log(row['start_time'] ~ " | " ~ row['action'] ~ " | " ~
           row['status'] ~ " | " ~ row['duration_s'] ~ "s | +" ~
           row['rows_inserted'] ~ "/-" ~ row['rows_deleted'] ~
           (" | ERR: " ~ row['error_message'] if row['error_message'] else ""),
           info=true) }}
  {% endfor %}
{% endmacro %}
```

---

## Phase 8 — Monitoring Integration

### 8.1 Exposures for stream table metadata

Users can define dbt exposures that reference stream tables for documentation:

```yaml
# models/exposures.yml
exposures:
  - name: real_time_dashboard
    type: dashboard
    depends_on:
      - ref('order_totals')
    description: "Customer order totals, refreshed every 5 minutes via pg_trickle"
    meta:
      pgtrickle_schedule: '5m'
      pgtrickle_refresh_mode: DIFFERENTIAL
```

### 8.2 Custom dbt tests for stream table health

```sql
-- tests/generic/test_pgtrickle_not_stale.sql
{% test pgtrickle_not_stale(model) %}
  SELECT pgt_name
  FROM pgtrickle.pg_stat_stream_tables
  WHERE pgt_name = '{{ model.identifier }}'
    AND stale = true
{% endtest %}

-- tests/generic/test_pgtrickle_no_errors.sql
{% test pgtrickle_no_errors(model) %}
  SELECT pgt_name
  FROM pgtrickle.pg_stat_stream_tables
  WHERE pgt_name = '{{ model.identifier }}'
    AND consecutive_errors > 0
{% endtest %}
```

Usage in schema YAML:

```yaml
models:
  - name: order_totals
    tests:
      - pgtrickle_not_stale
      - pgtrickle_no_errors
```

---

## Phase 9 — Testing

### 9.1 Unit tests

Test adapter methods in isolation with mocked database connections:

```python
# tests/unit/test_adapter.py

class TestPgTrickleAdapter:
    def test_get_columns_filters_pgt_row_id(self):
        """__pgt_row_id should be excluded from stream table columns."""
        ...

    def test_list_relations_classifies_stream_tables(self):
        """Stream tables should have type 'stream_table'."""
        ...

    def test_pgtrickle_get_stream_table_info_returns_none(self):
        """Returns None for non-existent stream tables."""
        ...
```

### 9.2 Functional tests

Use dbt's standard functional test framework with a PostgreSQL 18 + pg_trickle instance:

```python
# tests/functional/test_stream_table_materialization.py

class TestStreamTableMaterialization:
    """End-to-end tests for the stream_table materialization."""

    @pytest.fixture(scope="class")
    def models(self):
        return {
            "order_totals.sql": """
                {{
                  config(
                    materialized='stream_table',
                    schedule='30s',
                    refresh_mode='DIFFERENTIAL'
                  )
                }}
                SELECT customer_id, SUM(amount) AS total
                FROM {{ source('raw', 'orders') }}
                GROUP BY customer_id
            """,
            "schema.yml": """
                sources:
                  - name: raw
                    tables:
                      - name: orders
            """,
        }

    def test_first_run_creates_stream_table(self, project):
        """First dbt run should create the stream table."""
        results = run_dbt(["run"])
        assert len(results) == 1
        assert results[0].status == "success"
        # Verify ST exists in catalog
        result = project.run_sql(
            "SELECT 1 FROM pgtrickle.pgt_stream_tables "
            "WHERE pgt_name = 'order_totals'",
            fetch="one",
        )
        assert result is not None

    def test_second_run_is_noop(self, project):
        """Second run with same query should be a no-op."""
        run_dbt(["run"])
        results = run_dbt(["run"])
        assert len(results) == 1
        # Verify only one ST entry (not duplicated)

    def test_full_refresh_recreates(self, project):
        """--full-refresh should drop and recreate."""
        run_dbt(["run"])
        results = run_dbt(["run", "--full-refresh"])
        assert len(results) == 1
        assert results[0].status == "success"

    def test_query_change_triggers_recreate(self, project):
        """Changing the model SQL should drop and recreate."""
        run_dbt(["run"])
        # Modify model query (add a column)
        write_file(
            """
            {{ config(materialized='stream_table', schedule='30s') }}
            SELECT customer_id, SUM(amount) AS total, COUNT(*) AS cnt
            FROM {{ source('raw', 'orders') }}
            GROUP BY customer_id
            """,
            project.project_root,
            "models",
            "order_totals.sql",
        )
        results = run_dbt(["run"])
        assert results[0].status == "success"

    def test_columns_exclude_pgt_row_id(self, project):
        """__pgt_row_id should not appear in column introspection."""
        run_dbt(["run"])
        columns = project.run_sql(
            "SELECT column_name FROM information_schema.columns "
            "WHERE table_name = 'order_totals'",
            fetch="all",
        )
        col_names = [c[0] for c in columns]
        # Adapter should filter this out
        # (Note: information_schema still shows it; adapter filters at Python level)
```

### 9.3 Test infrastructure

Tests use the project's existing `tests/Dockerfile.e2e` to get a PostgreSQL 18 instance
with pg_trickle. The CI pipeline builds the image and exposes it for pytest.

---

## Phase 10 — Packaging & Distribution

### 10.1 PyPI publication

```bash
# Build
python -m build

# Publish
twine upload dist/*
```

### 10.2 Installation

```bash
pip install dbt-pgtrickle
```

### 10.3 Versioning

Follow semantic versioning aligned with dbt Core major versions:
- `dbt-pgtrickle 0.1.x` → dbt Core 1.7+
- `dbt-pgtrickle 1.0.x` → first stable release

---

## Comparison with Option A

| Aspect | Option A (Macro Package) ✅ Implemented | Option B (Full Adapter) |
|--------|----------------------------------------|------------------------|
| **Effort** | ~15 hours (done) | ~54 hours |
| **Dependencies** | dbt-postgres only | Custom Python package |
| **Installation** | `dbt deps` (git/Hub) | `pip install dbt-pgtrickle` |
| **`__pgt_row_id` hidden** | No (visible in docs) | Yes (filtered in adapter) |
| **Relation type** | Shows as `table` | Shows as `stream_table` |
| **Source freshness** | `pgtrickle_check_freshness` run-operation | Native `dbt source freshness` |
| **Custom operations** | refresh, drop, CDC health, freshness | + explain, refresh history |
| **Catalog integration** | Standard postgres | Enhanced with ST metadata |
| **Profile type** | `postgres` | `pgtrickle` (verifies extension) |
| **Testing** | dbt project tests + CI matrix (1.6–1.9) | Python unit + functional tests |
| **Maintenance** | Low (Jinja only) | Higher (Python + Jinja) |
| **CI** | ✅ dbt-integration job in ci.yml | Extends existing CI |

**Current status:** Option A is implemented and running in CI. Migrate to Option B when
stream tables are a central part of the data platform and users need first-class IDE
support, hidden `__pgt_row_id` columns in docs, and native `dbt source freshness`.

---

## File Layout

> **Migration note:** The `dbt-pgtrickle/` directory currently contains the
> Option A macro package. Implementing this adapter plan would restructure
> the directory into a Python package, moving existing macros under
> `dbt/include/pgtrickle/macros/` and adding the Python adapter code.
> The integration tests would move into `tests/functional/`.

```
dbt-pgtrickle/
├── pyproject.toml                                    # Package metadata, entry points
├── setup.py
├── README.md
├── LICENSE
├── CHANGELOG.md
├── dbt/
│   ├── __init__.py
│   ├── adapters/
│   │   ├── __init__.py
│   │   └── pgtrickle/
│   │       ├── __init__.py                          # Plugin registration
│   │       ├── connections.py                       # ~40 lines
│   │       ├── impl.py                              # ~250 lines (core adapter)
│   │       ├── relation.py                          # ~30 lines
│   │       ├── column.py                            # ~15 lines
│   │       └── pgtrickle_credentials.py              # ~15 lines
│   └── include/
│       └── pgtrickle/
│           ├── dbt_project.yml
│           ├── profile_template.yml
│           └── macros/
│               ├── materializations/
│               │   └── stream_table.sql             # ~80 lines
│               ├── adapters/
│               │   └── pgtrickle_api.sql             # ~50 lines
│               ├── catalog.sql                      # ~60 lines
│               ├── relations.sql                    # ~20 lines
│               ├── operations/
│               │   ├── refresh.sql                  # ~5 lines
│               │   ├── explain.sql                  # ~15 lines
│               │   ├── cdc_health.sql               # ~15 lines
│               │   └── refresh_history.sql          # ~20 lines
│               └── tests/
│                   ├── pgtrickle_not_stale.sql        # ~8 lines
│                   └── pgtrickle_no_errors.sql        # ~8 lines
└── tests/
    ├── conftest.py
    ├── unit/
    │   ├── test_adapter.py                          # ~100 lines
    │   └── test_relation.py                         # ~50 lines
    └── functional/
        ├── test_stream_table_materialization.py      # ~150 lines
        ├── test_column_filtering.py                  # ~50 lines
        └── test_freshness.py                         # ~80 lines
```

**Estimated total code:** ~350 lines Python + ~280 lines Jinja SQL + ~430 lines tests.

---

## Appendix: Adapter API Surface

### Python methods added to `PgTrickleAdapter`

| Method | Purpose |
|--------|---------|
| `pgtrickle_create_stream_table(name, query, schedule, mode, init)` | Create a stream table |
| `pgtrickle_drop_stream_table(name)` | Drop a stream table |
| `pgtrickle_alter_if_changed(name, schedule, mode, current)` | Alter schedule/mode if changed |
| `pgtrickle_refresh_stream_table(name)` | Trigger manual refresh |
| `pgtrickle_get_stream_table_info(name)` → `dict` | Read ST metadata from catalog |
| `get_columns_in_relation(relation)` | Override: filters `__pgt_row_id` |
| `list_relations_without_caching(schema)` | Override: classifies stream tables |
| `calculate_freshness(source, ...)` | Override: uses pg_trickle monitoring |

### Jinja macros

| Macro | Purpose |
|-------|---------|
| `materialization stream_table` | Core materialization |
| `pgtrickle__get_catalog_relations` | Catalog with ST metadata |
| `refresh(model_name)` | Run-operation: manual refresh |
| `explain(model_name)` | Run-operation: explain DVM plan |
| `cdc_health()` | Run-operation: CDC health check |
| `refresh_history(model_name, limit)` | Run-operation: refresh audit log |
| `test_pgtrickle_not_stale` | Generic test: staleness |
| `test_pgtrickle_no_errors` | Generic test: error streak |

---

## Effort Estimate

| Phase | Effort |
|-------|--------|
| Phase 1 — Scaffolding | 4 hours |
| Phase 2 — Connection & Credentials | 2 hours |
| Phase 3 — Relation Types & Catalog | 8 hours |
| Phase 4 — Custom Materialization | 8 hours |
| Phase 5 — Column Introspection | 4 hours |
| Phase 6 — Source Freshness | 4 hours |
| Phase 7 — Custom Operations | 4 hours |
| Phase 8 — Monitoring Integration | 4 hours |
| Phase 9 — Testing | 12 hours |
| Phase 10 — Packaging & Distribution | 4 hours |
| **Total** | **~54 hours** |
