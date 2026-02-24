# Plan: dbt Integration via Custom Materialization Macro

**Option A — dbt Package with Custom Materialization**

Date: 2026-02-24
Status: PROPOSED

---

## Overview

Implement pg_stream integration with [dbt Core](https://docs.getdbt.com/docs/introduction)
as a **dbt package** containing a custom materialization macro (`stream_table`). This approach
requires no Python adapter code — just Jinja SQL macros that call pg_stream's SQL API functions.
It works with the standard `dbt-postgres` adapter.

The package lives **inside the pg_stream repository** as the `dbt-pgstream/` subfolder.
This keeps the macro co-located with the extension source, enables single-PR changes when
the SQL API evolves, and lets CI test the macros against the actual extension in one pipeline.
Users install it via a git URL with the `subdirectory` key in their `packages.yml`.

This is the lighter-weight option compared to a full dbt adapter (see
[PLAN_DBT_ADAPTER.md](PLAN_DBT_ADAPTER.md)). It covers the core workflow (create, update,
drop, test) and is suitable for teams that want to manage stream tables alongside their
existing dbt models.

---

## Table of Contents

- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Phase 1 — Package Scaffolding](#phase-1--package-scaffolding)
- [Phase 2 — Custom Materialization](#phase-2--custom-materialization)
- [Phase 3 — Model Configuration](#phase-3--model-configuration)
- [Phase 4 — Lifecycle Hooks](#phase-4--lifecycle-hooks)
- [Phase 5 — Source Freshness Integration](#phase-5--source-freshness-integration)
- [Phase 6 — Testing & Documentation](#phase-6--testing--documentation)
- [Limitations](#limitations)
- [File Layout](#file-layout)
- [Appendix: Example Project](#appendix-example-project)

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                     dbt Core (CLI)                       │
│                                                          │
│  dbt run → materialization macro → SQL function calls    │
│  dbt test → standard test runner (heap table queries)    │
│  dbt source freshness → custom macro → monitoring view   │
└──────────────────┬───────────────────────────────────────┘
                   │  Standard dbt-postgres adapter
                   ▼
┌──────────────────────────────────────────────────────────┐
│                   PostgreSQL 18                          │
│                                                          │
│  pgstream.create_stream_table(name, query, schedule,     │
│                                refresh_mode, initialize)│
│  pgstream.alter_stream_table(name, ...)                 │
│  pgstream.drop_stream_table(name)                       │
│  pgstream.refresh_stream_table(name)                    │
│  pgstream.pg_stat_stream_tables  (monitoring view)      │
│  pgstream.check_cdc_health()     (health function)      │
└──────────────────────────────────────────────────────────┘
```

The key insight is that pg_stream's entire API is SQL function calls, not DDL. A dbt
custom materialization can wrap these calls in Jinja macros and map dbt's lifecycle
(create → run → test → teardown) onto them.

---

## Prerequisites

- dbt Core ≥ 1.6 (required for `subdirectory` support in `packages.yml`)
- `dbt-postgres` adapter (standard; no custom adapter needed)
- PostgreSQL 18 with pg_stream extension installed
- The dbt execution role needs permission to call `pgstream.*` functions

---

## Phase 1 — Package Scaffolding

### 1.1 Location within the pg_stream repo

The dbt package lives as a subfolder in the main pg_stream repository. This avoids a
separate repo, keeps the SQL API and macros in sync, and lets CI test both together.

```
pg-stream/                            # Main extension repo
├── src/                              # Rust extension source
├── tests/                            # Extension tests
├── docs/
├── dbt-pgstream/                     # ← dbt macro package (subfolder)
│   ├── dbt_project.yml
│   ├── README.md
│   ├── macros/
│   │   ├── materializations/
│   │   │   └── stream_table.sql      # Core materialization
│   │   ├── adapters/
│   │   │   ├── create_stream_table.sql
│   │   │   ├── alter_stream_table.sql
│   │   │   ├── drop_stream_table.sql
│   │   │   └── refresh_stream_table.sql
│   │   ├── hooks/
│   │   │   └── source_freshness.sql
│   │   ├── operations/
│   │   │   ├── refresh.sql
│   │   │   └── drop_all.sql
│   │   └── utils/
│   │       ├── stream_table_exists.sql
│   │       └── get_stream_table_info.sql
│   └── integration_tests/
│       ├── dbt_project.yml
│       ├── profiles.yml
│       ├── models/
│       │   └── marts/
│       │       ├── order_totals.sql
│       │       └── schema.yml
│       ├── seeds/
│       │   └── raw_orders.csv
│       └── tests/
│           └── assert_totals_correct.sql
├── AGENTS.md
├── Cargo.toml
└── ...
```

### 1.2 User installation

Users install the package via a git URL with the `subdirectory` key (dbt Core ≥ 1.6):

```yaml
# packages.yml (in the user's dbt project)
packages:
  - git: "https://github.com/<org>/pg-stream.git"
    revision: v0.1.0    # git tag, branch, or commit SHA
    subdirectory: "dbt-pgstream"
```

Then run:

```bash
dbt deps   # clones pg-stream repo, installs only dbt-pgstream/ subfolder
```

> **Note:** `dbt deps` performs a shallow clone by default, so pulling the full Rust
> source tree adds only a few MB of transfer — acceptable for most users.

### 1.3 Why in-repo, not separate?

| Concern | In-repo subfolder | Separate repo |
|---------|--------------------|---------------|
| Single PR for API + macro changes | ✅ Yes | ❌ Two PRs |
| Shared CI (test macros against extension) | ✅ Same pipeline | ❌ Cross-repo trigger |
| Version tags track both | ✅ One tag | ❌ Separate tags |
| Contributor experience | ✅ One clone | ❌ Two repos |
| `dbt deps` payload | ~few MB extra (shallow clone) | Minimal |
| dbt Hub publication | Possible with `subdirectory` | Easier (root `dbt_project.yml`) |

If the package later needs dbt Hub publication or grows into a full adapter (Python on
PyPI), it can be extracted to a separate repo at that point.

### 1.4 dbt_project.yml

```yaml
# dbt-pgstream/dbt_project.yml
name: 'dbt_pgstream'
version: '0.1.0'
config-version: 2

require-dbt-version: [">=1.6.0", "<2.0.0"]  # ≥1.6 for subdirectory support
```

---

## Phase 2 — Custom Materialization

### 2.1 Materialization entry point

File: `macros/materializations/stream_table.sql`

The materialization must handle three cases:

1. **First run** — stream table does not exist → call `create_stream_table()`
2. **Subsequent run** — stream table exists, query unchanged → no-op (or update schedule/mode)
3. **Full refresh** (`dbt run --full-refresh`) — drop and recreate

```sql
{% materialization stream_table, adapter='postgres' %}

  {%- set target_relation = this.incorporate(type='table') -%}
  {%- set existing_relation = load_cached_relation(this) -%}

  {# -- Model config -- #}
  {%- set schedule = config.get('schedule', '1m') -%}
  {%- set refresh_mode = config.get('refresh_mode', 'DIFFERENTIAL') -%}
  {%- set initialize = config.get('initialize', true) -%}
  {%- set st_name = config.get('stream_table_name', target_relation.identifier) -%}
  {%- set st_schema = config.get('stream_table_schema', target_relation.schema) -%}
  {%- set full_refresh_mode = (flags.FULL_REFRESH == True) -%}

  {# -- Determine the fully-qualified stream table name -- #}
  {%- set qualified_name = st_schema ~ '.' ~ st_name
        if st_schema != 'public'
        else st_name -%}

  {{ run_hooks(pre_hooks) }}

  {# -- Full refresh: drop and recreate -- #}
  {% if full_refresh_mode and existing_relation is not none %}
    {{ pgstream_drop_stream_table(qualified_name) }}
    {% set existing_relation = none %}
  {% endif %}

  {# -- Get the compiled SQL (the defining query) -- #}
  {%- set defining_query = sql -%}

  {% if existing_relation is none %}
    {# -- CREATE: stream table does not exist yet -- #}
    {{ pgstream_create_stream_table(
         qualified_name, defining_query, schedule, refresh_mode, initialize
       ) }}
    {% do adapter.cache_new(this.incorporate(type='table')) %}
  {% else %}
    {# -- UPDATE: stream table exists — check if query changed -- #}
    {%- set current_info = pgstream_get_stream_table_info(qualified_name) -%}

    {% if current_info and current_info.defining_query != defining_query %}
      {# Query changed: must drop and recreate #}
      {{ pgstream_drop_stream_table(qualified_name) }}
      {{ pgstream_create_stream_table(
           qualified_name, defining_query, schedule, refresh_mode, initialize
         ) }}
    {% else %}
      {# Query unchanged: update schedule/mode if they differ #}
      {{ pgstream_alter_stream_table(
           qualified_name, schedule, refresh_mode
         ) }}
    {% endif %}
  {% endif %}

  {{ run_hooks(post_hooks) }}

  {{ return({'relations': [target_relation]}) }}

{% endmaterialization %}
```

### 2.2 Existence check helper

File: `macros/utils/stream_table_exists.sql`

```sql
{% macro pgstream_stream_table_exists(name) %}
  {% set query %}
    SELECT EXISTS(
      SELECT 1 FROM pgstream.pgs_stream_tables
      WHERE pgs_name = '{{ name }}'
    ) AS st_exists
  {% endset %}
  {% set result = run_query(query) %}
  {% if result and result.rows %}
    {{ return(result.rows[0]['st_exists']) }}
  {% else %}
    {{ return(false) }}
  {% endif %}
{% endmacro %}
```

### 2.3 Metadata reader helper

File: `macros/utils/get_stream_table_info.sql`

```sql
{% macro pgstream_get_stream_table_info(name) %}
  {% set query %}
    SELECT pgs_name, defining_query, schedule, refresh_mode, status
    FROM pgstream.pgs_stream_tables
    WHERE pgs_name = '{{ name }}'
  {% endset %}
  {% set result = run_query(query) %}
  {% if result and result.rows | length > 0 %}
    {{ return(result.rows[0]) }}
  {% else %}
    {{ return(none) }}
  {% endif %}
{% endmacro %}
```

### 2.4 SQL API wrapper macros

File: `macros/adapters/create_stream_table.sql`

```sql
{% macro pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize) %}
  {% set create_sql %}
    SELECT pgstream.create_stream_table(
      {{ dbt.string_literal(name) }},
      {{ dbt.string_literal(query) }},
      {{ dbt.string_literal(schedule) }},
      {{ dbt.string_literal(refresh_mode) }},
      {{ initialize }}
    )
  {% endset %}
  {% do run_query(create_sql) %}
  {{ log("Created stream table: " ~ name, info=true) }}
{% endmacro %}
```

File: `macros/adapters/alter_stream_table.sql`

```sql
{% macro pgstream_alter_stream_table(name, schedule, refresh_mode) %}
  {# Only alter if schedule or mode differ from current #}
  {% set current = pgstream_get_stream_table_info(name) %}
  {% if current %}
    {% if current.schedule != schedule %}
      {% set alter_sql %}
        SELECT pgstream.alter_stream_table(
          {{ dbt.string_literal(name) }},
          schedule => {{ dbt.string_literal(schedule) }}
        )
      {% endset %}
      {% do run_query(alter_sql) %}
      {{ log("Updated schedule for " ~ name ~ " to " ~ schedule, info=true) }}
    {% endif %}

    {% if current.refresh_mode != refresh_mode %}
      {% set alter_sql %}
        SELECT pgstream.alter_stream_table(
          {{ dbt.string_literal(name) }},
          refresh_mode => {{ dbt.string_literal(refresh_mode) }}
        )
      {% endset %}
      {% do run_query(alter_sql) %}
      {{ log("Updated refresh_mode for " ~ name ~ " to " ~ refresh_mode, info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
```

File: `macros/adapters/drop_stream_table.sql`

```sql
{% macro pgstream_drop_stream_table(name) %}
  {% set drop_sql %}
    SELECT pgstream.drop_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(drop_sql) %}
  {{ log("Dropped stream table: " ~ name, info=true) }}
{% endmacro %}
```

File: `macros/adapters/refresh_stream_table.sql`

```sql
{% macro pgstream_refresh_stream_table(name) %}
  {% set refresh_sql %}
    SELECT pgstream.refresh_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(refresh_sql) %}
  {{ log("Refreshed stream table: " ~ name, info=true) }}
{% endmacro %}
```

---

## Phase 3 — Model Configuration

### 3.1 Model-level config

Users configure stream tables via dbt model config:

```yaml
# models/marts/order_totals.yml
models:
  - name: order_totals
    config:
      materialized: stream_table
      schedule: '5m'
      refresh_mode: DIFFERENTIAL
      initialize: true
```

Or inline in the model SQL file:

```sql
-- models/marts/order_totals.sql
{{
  config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='DIFFERENTIAL'
  )
}}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ source('raw', 'orders') }}
GROUP BY customer_id
```

### 3.2 Supported config keys

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `materialized` | string | — | Must be `'stream_table'` |
| `schedule` | string | `'1m'` | Refresh schedule (duration or cron). Passed directly to `create_stream_table()`. |
| `refresh_mode` | string | `'DIFFERENTIAL'` | `'FULL'` or `'DIFFERENTIAL'`. |
| `initialize` | bool | `true` | Whether to populate on creation. |
| `stream_table_name` | string | model name | Override the stream table name if it differs from the dbt model name. |
| `stream_table_schema` | string | target schema | Override the schema. |

### 3.3 Project-level defaults

```yaml
# dbt_project.yml
models:
  my_project:
    marts:
      +materialized: stream_table
      +schedule: '5m'
      +refresh_mode: DIFFERENTIAL
```

---

## Phase 4 — Lifecycle Hooks

### 4.1 `dbt run` behavior

| Scenario | Action |
|----------|--------|
| ST does not exist | `create_stream_table()` with compiled SQL as defining query |
| ST exists, query unchanged | `alter_stream_table()` if schedule or mode changed; no-op otherwise |
| ST exists, query changed | `drop_stream_table()` + `create_stream_table()` |
| `--full-refresh` flag | `drop_stream_table()` + `create_stream_table()` regardless |

### 4.2 `dbt run-operation`

Expose a run-operation for manual refresh:

File: `macros/operations/refresh.sql`

```sql
{% macro refresh(model_name) %}
  {{ pgstream_refresh_stream_table(model_name) }}
{% endmacro %}
```

Usage:
```bash
dbt run-operation refresh --args '{"model_name": "order_totals"}'
```

### 4.3 `dbt test`

No special handling needed. Stream tables are standard PostgreSQL heap tables. All dbt
tests (schema tests, data tests, custom tests) work normally by querying the table.

The `__pgs_row_id` column is present but does not interfere with tests unless the user
explicitly selects `*` and checks column counts. Document this in the README.

### 4.4 `dbt docs generate`

dbt introspects tables via `information_schema`. The `__pgs_row_id` column will appear
in the generated docs. Add a post-hook or custom docs macro to annotate it:

```yaml
# models/marts/order_totals.yml
models:
  - name: order_totals
    columns:
      - name: __pgs_row_id
        description: "Internal pg_stream row identity hash. Ignore this column."
```

### 4.5 `dbt clean` / teardown

dbt does not have a native "teardown" command. To drop all stream tables managed by dbt,
provide a run-operation:

```sql
{% macro drop_all_stream_tables() %}
  {% set models = graph.nodes.values()
       | selectattr('config.materialized', 'equalto', 'stream_table') %}
  {% for model in models %}
    {% set name = model.config.get('stream_table_name', model.name) %}
    {% if pgstream_stream_table_exists(name) %}
      {{ pgstream_drop_stream_table(name) }}
    {% endif %}
  {% endfor %}
{% endmacro %}
```

---

## Phase 5 — Source Freshness Integration

### 5.1 Mapping to dbt source freshness

dbt's `dbt source freshness` checks `loaded_at_field` timestamps. pg_stream has native
staleness tracking via `pgstream.pg_stat_stream_tables`. We can bridge these by
overriding the freshness check for stream-table sources.

File: `macros/hooks/source_freshness.sql`

```sql
{% macro pgstream_source_freshness(source_name) %}
  {# Returns freshness data from pg_stream's monitoring view #}
  {% set query %}
    SELECT
      pgs_name,
      last_refresh_at,
      staleness,
      stale,
      EXTRACT(EPOCH FROM staleness)::int AS staleness_seconds
    FROM pgstream.pg_stat_stream_tables
    WHERE pgs_name = '{{ source_name }}'
  {% endset %}
  {% set result = run_query(query) %}
  {% if result and result.rows | length > 0 %}
    {{ return(result.rows[0]) }}
  {% endif %}
{% endmacro %}
```

### 5.2 Source definition example

```yaml
sources:
  - name: pgstream
    schema: public
    freshness:
      warn_after: {count: 10, period: minute}
      error_after: {count: 30, period: minute}
    loaded_at_field: "last_refresh_at"
    tables:
      - name: order_totals
        # dbt source freshness will check last_refresh_at automatically
```

Since `last_refresh_at` is stored in `pgstream.pgs_stream_tables`, you can create a view
that exposes it on the stream table itself, or reference the monitoring view directly.

---

## Phase 6 — Testing & Documentation

### 6.1 Integration tests

The `integration_tests/` directory is a standalone dbt project that:

1. Seeds test data (`raw_orders.csv`)
2. Runs `dbt run` to create stream tables
3. Runs `dbt test` to verify data correctness
4. Runs `dbt run --full-refresh` to verify drop/recreate
5. Modifies the model query and re-runs to verify the alter path

Tests require PostgreSQL 18 with pg_stream installed. Use the project's existing
`tests/Dockerfile.e2e` as the test environment.

### 6.2 CI pipeline

Since the macros live in the pg_stream repo, dbt integration tests run as part of the
main CI pipeline alongside the Rust extension tests:

```yaml
# In the main .github/workflows/ci.yml (or a dedicated job)
dbt-integration:
  runs-on: ubuntu-latest
  needs: [build]   # Ensure the pg_stream Docker image is built first
  services:
    postgres:
      image: pg-stream-e2e:latest  # Custom image with pg_stream
      ports: ['5432:5432']
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-python@v5
      with: { python-version: '3.11' }
    - run: pip install dbt-core dbt-postgres
    - run: cd dbt-pgstream/integration_tests && dbt deps && dbt seed && dbt run && dbt test
    - run: cd dbt-pgstream/integration_tests && dbt run --full-refresh  # test drop/recreate
```

This ensures that any SQL API change in the Rust extension is immediately validated
against the dbt macros in the same PR.

### 6.3 Documentation

- **`dbt-pgstream/README.md`** — Quick start, installation (`subdirectory` git URL), configuration examples
- **CHANGELOG.md** — Version history (tracked via the main repo's tags)
- Inline Jinja doc comments on all macros

---

## Limitations

| Limitation | Explanation | Workaround |
|------------|-------------|------------|
| No in-place query alteration | `alter_stream_table()` cannot change the defining query; must drop/recreate | The materialization handles this automatically |
| `__pgs_row_id` visible | Internal column appears in `SELECT *` and dbt docs | Document it; exclude in downstream models |
| No `dbt snapshot` support | Snapshots use SCD Type-2 logic that doesn't apply to stream tables | Use a separate snapshot on the stream table as a regular table |
| No cross-database refs | Stream tables live in the same database as sources | Standard PostgreSQL limitation |
| Concurrent `dbt run` | Multiple `dbt run` invocations could race on create/drop | Use dbt's `--target` or coordinate via CI |
| `dbt deps` payload | Users clone the full pg_stream repo (shallow, ~few MB) | Use `subdirectory` key; acceptable tradeoff |

---

## File Layout

Within the pg_stream repository:

```
pg-stream/
├── src/                                  # Rust extension source
├── tests/                                # Extension tests
├── dbt-pgstream/                         # ← dbt macro package
│   ├── dbt_project.yml                   # Package manifest
│   ├── README.md                         # Quick start, installation
│   ├── macros/
│   │   ├── materializations/
│   │   │   └── stream_table.sql          # ~80 lines
│   │   ├── adapters/
│   │   │   ├── create_stream_table.sql   # ~15 lines
│   │   │   ├── alter_stream_table.sql    # ~25 lines
│   │   │   ├── drop_stream_table.sql     # ~8 lines
│   │   │   └── refresh_stream_table.sql  # ~8 lines
│   │   ├── hooks/
│   │   │   └── source_freshness.sql      # ~20 lines
│   │   ├── operations/
│   │   │   ├── refresh.sql               # ~5 lines
│   │   │   └── drop_all.sql              # ~15 lines
│   │   └── utils/
│   │       ├── stream_table_exists.sql   # ~12 lines
│   │       └── get_stream_table_info.sql # ~12 lines
│   └── integration_tests/
│       ├── dbt_project.yml
│       ├── profiles.yml
│       ├── models/
│       │   └── marts/
│       │       ├── order_totals.sql
│       │       └── schema.yml
│       ├── seeds/
│       │   └── raw_orders.csv
│       └── tests/
│           └── assert_totals_correct.sql
├── Cargo.toml
└── ...
```

**Estimated total code:** ~200 lines of Jinja SQL macros + ~100 lines of test/config YAML.

> No `.github/workflows/` directory inside `dbt-pgstream/` — CI lives in the main repo's
> workflow files and includes a `dbt-integration` job.

---

## Appendix: Example Project

### Source table (pre-existing)

```sql
CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    customer_id INT NOT NULL,
    amount NUMERIC(10,2) NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now()
);
```

### dbt model

```sql
-- models/marts/order_totals.sql
{{
  config(
    materialized='stream_table',
    schedule='5m',
    refresh_mode='DIFFERENTIAL'
  )
}}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ source('raw', 'orders') }}
GROUP BY customer_id
```

### Install the package

```yaml
# packages.yml (in the user's dbt project)
packages:
  - git: "https://github.com/<org>/pg-stream.git"
    revision: v0.1.0
    subdirectory: "dbt-pgstream"
```

```bash
dbt deps
```

### dbt commands

```bash
# First run: creates the stream table
dbt run --select order_totals

# Verify data
dbt test --select order_totals

# Manual one-off refresh
dbt run-operation refresh --args '{"model_name": "order_totals"}'

# Force drop + recreate
dbt run --select order_totals --full-refresh

# Check freshness
dbt source freshness --select source:raw

# Tear down all stream tables
dbt run-operation drop_all_stream_tables
```

---

## Effort Estimate

| Phase | Effort |
|-------|--------|
| Phase 1 — Scaffolding | 2 hours |
| Phase 2 — Materialization | 4 hours |
| Phase 3 — Model config | 1 hour |
| Phase 4 — Lifecycle hooks | 2 hours |
| Phase 5 — Source freshness | 2 hours |
| Phase 6 — Testing & docs | 4 hours |
| **Total** | **~15 hours** |
