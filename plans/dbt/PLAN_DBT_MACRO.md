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
- [Phase 2 — SQL API Wrappers](#phase-2--sql-api-wrappers)
- [Phase 3 — Utility Macros](#phase-3--utility-macros)
- [Phase 4 — Custom Materialization](#phase-4--custom-materialization)
- [Phase 5 — Model Configuration](#phase-5--model-configuration)
- [Phase 6 — Lifecycle Operations](#phase-6--lifecycle-operations)
- [Phase 7 — Source Freshness Integration](#phase-7--source-freshness-integration)
- [Phase 8 — Integration Tests](#phase-8--integration-tests)
- [Phase 9 — CI Pipeline](#phase-9--ci-pipeline)
- [Phase 10 — Documentation](#phase-10--documentation)
- [pg-stream SQL API Reference](#pg-stream-sql-api-reference)
- [Limitations](#limitations)
- [File Layout](#file-layout)
- [Effort Estimate](#effort-estimate)
- [Appendix: Example Project](#appendix-example-project)

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                      dbt Core (CLI)                          │
│                                                              │
│  packages.yml ─→ dbt deps ─→ installs dbt-pgstream macros   │
│                                                              │
│  dbt run ──────→ stream_table materialization                │
│                    ├─ create_stream_table()                   │
│                    ├─ alter_stream_table()                    │
│                    └─ drop_stream_table()                     │
│  dbt test ─────→ standard test runner (heap table queries)   │
│  dbt source freshness → see Phase 7 (custom run-operation)   │
│  dbt run-operation ─→ pgstream_refresh / drop_all / freshness│
└──────────────────┬───────────────────────────────────────────┘
                   │  Standard dbt-postgres adapter (no custom adapter)
                   ▼
┌──────────────────────────────────────────────────────────────┐
│                   PostgreSQL 18 + pg_stream                  │
│                                                              │
│  pgstream.create_stream_table(name, query, schedule,         │
│                                refresh_mode, initialize)     │
│  pgstream.alter_stream_table(name, ...)                      │
│  pgstream.drop_stream_table(name)                            │
│  pgstream.refresh_stream_table(name)                         │
│  pgstream.pg_stat_stream_tables   (monitoring view)          │
│  pgstream.pgs_stream_tables       (catalog table)            │
│  pgstream.check_cdc_health()      (health function)          │
└──────────────────────────────────────────────────────────────┘
```

The key insight is that pg_stream's entire API is SQL function calls, not DDL. A dbt
custom materialization can wrap these calls in Jinja macros and map dbt's lifecycle
(create → run → test → teardown) onto them.

---

## Prerequisites

| Requirement | Minimum Version | Notes |
|-------------|----------------|-------|
| dbt Core | ≥ 1.6 | Required for `subdirectory` support in `packages.yml` |
| dbt-postgres adapter | Matching dbt Core version | Standard adapter; no custom adapter needed |
| PostgreSQL | 18.x | pg_stream extension requires PG 18 |
| pg_stream extension | ≥ 0.1.0 | `CREATE EXTENSION pg_stream;` must succeed |
| dbt execution role | — | Needs permission to call `pgstream.*` functions |

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

macro-paths: ["macros"]
clean-targets:
  - "target"
  - "dbt_packages"
```

---

## Phase 2 — SQL API Wrappers

These macros provide thin, safe wrappers around pg_stream's SQL API functions. They are
used by the materialization (Phase 4) and lifecycle operations (Phase 6).

All wrappers use `dbt.string_literal()` for safe quoting and `run_query()` for execution.

> **Error handling:** If any wrapper's `run_query()` call fails (e.g., invalid query,
> permission denied, duplicate name), dbt surfaces the PostgreSQL error as a
> `DatabaseException`. The wrapper macros log the operation being attempted so that
> error messages have context. For production use, consider wrapping critical calls
> in `{% call statement(...) %}` blocks with explicit error messages.

### 2.1 `create_stream_table`

File: `macros/adapters/create_stream_table.sql`

Note: `schedule` may be `none` if the user wants pg_stream's CALCULATED schedule.
The pg_stream SQL API accepts `NULL` for schedule, which triggers automatic calculation.

```sql
{% macro pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize) %}
  {% set create_sql %}
    SELECT pgstream.create_stream_table(
      {{ dbt.string_literal(name) }},
      {{ dbt.string_literal(query) }},
      {% if schedule is none %}NULL{% else %}{{ dbt.string_literal(schedule) }}{% endif %},
      {{ dbt.string_literal(refresh_mode) }},
      {{ initialize }}
    )
  {% endset %}
  {% do run_query(create_sql) %}
  {{ log("pg_stream: created stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
```

### 2.2 `alter_stream_table`

File: `macros/adapters/alter_stream_table.sql`

Pass `NULL` for parameters that should remain unchanged. The pg_stream API treats `NULL`
as "keep current value".

Accepts an optional `current_info` parameter to avoid a redundant catalog lookup when
the materialization has already fetched the metadata.

```sql
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
          schedule => {% if current.schedule != schedule %}{{ dbt.string_literal(schedule) }}{% else %}NULL{% endif %},
          refresh_mode => {% if current.refresh_mode != refresh_mode %}{{ dbt.string_literal(refresh_mode) }}{% else %}NULL{% endif %},
          status => {% if status is not none and current.status != status %}{{ dbt.string_literal(status) }}{% else %}NULL{% endif %}
        )
      {% endset %}
      {% do run_query(alter_sql) %}
      {{ log("pg_stream: altered stream table '" ~ name ~ "'", info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
```

### 2.3 `drop_stream_table`

File: `macros/adapters/drop_stream_table.sql`

```sql
{% macro pgstream_drop_stream_table(name) %}
  {% set drop_sql %}
    SELECT pgstream.drop_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(drop_sql) %}
  {{ log("pg_stream: dropped stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
```

### 2.4 `refresh_stream_table`

File: `macros/adapters/refresh_stream_table.sql`

```sql
{% macro pgstream_refresh_stream_table(name) %}
  {% set refresh_sql %}
    SELECT pgstream.refresh_stream_table({{ dbt.string_literal(name) }})
  {% endset %}
  {% do run_query(refresh_sql) %}
  {{ log("pg_stream: refreshed stream table '" ~ name ~ "'", info=true) }}
{% endmacro %}
```

---

## Phase 3 — Utility Macros

Helper macros for existence checks and metadata reads. These are used by the
materialization and lifecycle operations.

**Important:** All utility macros that run SQL must guard with `{% if execute %}` to
prevent parse-time execution. dbt parses all macros during compilation — without this
guard, `run_query()` would fire during `dbt parse` and fail if the database is
unavailable.

### 3.1 Existence check

File: `macros/utils/stream_table_exists.sql`

Handles both simple names (`order_totals`) and schema-qualified names
(`analytics.order_totals`) by splitting on `.` and matching against **both**
`pgs_schema` and `pgs_name` columns. This avoids ambiguity when two schemas
have a stream table with the same name.

```sql
{% macro pgstream_stream_table_exists(name) %}
  {% if execute %}
    {# Split schema-qualified name if present #}
    {% set parts = name.split('.') %}
    {% if parts | length == 2 %}
      {% set lookup_schema = parts[0] %}
      {% set lookup_name = parts[1] %}
    {% else %}
      {% set lookup_schema = 'public' %}
      {% set lookup_name = name %}
    {% endif %}

    {% set query %}
      SELECT EXISTS(
        SELECT 1 FROM pgstream.pgs_stream_tables
        WHERE pgs_schema = {{ dbt.string_literal(lookup_schema) }}
          AND pgs_name = {{ dbt.string_literal(lookup_name) }}
      ) AS st_exists
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows %}
      {{ return(result.rows[0]['st_exists']) }}
    {% endif %}
  {% endif %}
  {{ return(false) }}
{% endmacro %}
```

### 3.2 Metadata reader

File: `macros/utils/get_stream_table_info.sql`

Returns a row dict with `pgs_name`, `pgs_schema`, `defining_query`, `schedule`,
`refresh_mode`, `status` — or `none` if the stream table does not exist.
Filters on both `pgs_schema` and `pgs_name` to avoid ambiguity.

```sql
{% macro pgstream_get_stream_table_info(name) %}
  {% if execute %}
    {% set parts = name.split('.') %}
    {% if parts | length == 2 %}
      {% set lookup_schema = parts[0] %}
      {% set lookup_name = parts[1] %}
    {% else %}
      {% set lookup_schema = 'public' %}
      {% set lookup_name = name %}
    {% endif %}

    {% set query %}
      SELECT pgs_name, pgs_schema, defining_query, schedule, refresh_mode, status
      FROM pgstream.pgs_stream_tables
      WHERE pgs_schema = {{ dbt.string_literal(lookup_schema) }}
        AND pgs_name = {{ dbt.string_literal(lookup_name) }}
    {% endset %}
    {% set result = run_query(query) %}
    {% if result and result.rows | length > 0 %}
      {{ return(result.rows[0]) }}
    {% endif %}
  {% endif %}
  {{ return(none) }}
{% endmacro %}
```

---

## Phase 4 — Custom Materialization

### 4.1 Materialization entry point

File: `macros/materializations/stream_table.sql`

The materialization must handle three cases:

1. **First run** — stream table does not exist → call `create_stream_table()`
2. **Subsequent run** — stream table exists, query unchanged → no-op (or update schedule/mode)
3. **Full refresh** (`dbt run --full-refresh`) — drop and recreate

```sql
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
```

### 4.2 Design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| `adapter='postgres'` | Tie to postgres adapter | pg_stream only runs on PostgreSQL; avoids confusion with other adapters |
| `pgstream_stream_table_exists()` | Authoritative check via catalog | Correct even if stream table was created/dropped outside dbt (unlike `load_cached_relation`) |
| `dbt.string_literal()` | Safe quoting for all parameters | Prevents SQL injection from model configs |
| `flags.FULL_REFRESH` | Check dbt global flag | Standard way to detect `--full-refresh` flag |
| `run_hooks(pre_hooks)` / `run_hooks(post_hooks)` | Support dbt hooks | Allows users to add custom pre/post SQL |
| Pass `current_info` to alter | Avoid redundant catalog lookup | Materialization already fetched metadata; don't read it again in the alter wrapper |
| Always schema-qualify | `st_schema ~ '.' ~ st_name` | Consistent naming; avoids `public` special-casing edge cases |

### 4.3 Query change detection

The materialization compares the compiled SQL (`sql`) with the `defining_query` stored
in `pgstream.pgs_stream_tables`. If they differ, it drops and recreates the stream table.

**Known limitation:** String comparison is sensitive to whitespace differences. The same
logical query with different formatting will be treated as a change, triggering an
unnecessary drop/recreate.

**Future improvement:** pg_stream could expose a `pgs_query_hash` column in the catalog
that stores a normalized hash of the defining query. The materialization would then
compare hashes instead of raw strings. For now, the simple string comparison is
acceptable because:
- dbt compiles the query deterministically from the same model file
- Unnecessary recreations are safe (just briefly interrupt the refresh schedule)
- This matches how dbt's built-in `incremental` materialization detects schema changes

---

## Phase 5 — Model Configuration

### 5.1 Model-level config

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

### 5.2 Supported config keys

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `materialized` | string | — | Must be `'stream_table'` |
| `schedule` | string/null | `'1m'` | Refresh schedule (duration or cron). Set to `null` for pg_stream's CALCULATED schedule. Passed directly to `create_stream_table()`. |
| `refresh_mode` | string | `'DIFFERENTIAL'` | `'FULL'` or `'DIFFERENTIAL'`. |
| `initialize` | bool | `true` | Whether to populate on creation. |
| `status` | string/null | `null` (no change) | `'ACTIVE'` or `'PAUSED'`. When set, passed to `alter_stream_table()` on subsequent runs. Allows pausing/resuming a stream table from dbt config. |
| `stream_table_name` | string | model name | Override the stream table name if it differs from the dbt model name. |
| `stream_table_schema` | string | target schema | Override the schema. |

### 5.3 Project-level defaults

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

## Phase 6 — Lifecycle Operations

### 6.1 `dbt run` behavior

| Scenario | Action |
|----------|--------|
| ST does not exist | `create_stream_table()` with compiled SQL as defining query |
| ST exists, query unchanged | `alter_stream_table()` if schedule, mode, or status changed; no-op otherwise |
| ST exists, query changed | `drop_stream_table()` + `create_stream_table()` |
| `--full-refresh` flag | `drop_stream_table()` + `create_stream_table()` regardless |

### 6.1.1 `dbt build`

`dbt build` runs models and tests in DAG order. Since stream table models typically
reference raw source tables (not other dbt models), they tend to be scheduled early in
the DAG. This is fine — the materialization creates the stream table, and pg_stream's
background scheduler handles ongoing refreshes independently of dbt.

Note: if a standard dbt model depends on a stream table (via `ref()`), `dbt build` will
run the stream table materialization first, then the downstream model. The stream table
may not be populated yet if `initialize: false` is set — users should be aware of this
ordering.

### 6.2 Manual refresh

File: `macros/operations/refresh.sql`

Named `pgstream_refresh` (not just `refresh`) to avoid name collisions with other
packages or user macros.

```sql
{% macro pgstream_refresh(model_name) %}
  {{ pgstream_refresh_stream_table(model_name) }}
{% endmacro %}
```

Usage:
```bash
dbt run-operation pgstream_refresh --args '{"model_name": "order_totals"}'
```

### 6.3 Drop stream tables

File: `macros/operations/drop_all.sql`

Two macros are provided — the **default is the safe one** that only drops dbt-managed
stream tables. A separate "nuclear" option drops everything.

#### `drop_all_stream_tables` (default — dbt-managed only)

Drops only stream tables that correspond to dbt models with `materialized: stream_table`.
Safe in shared environments where non-dbt stream tables may exist.

```sql
{% macro drop_all_stream_tables() %}
  {% if execute %}
    {% set dropped = [] %}
    {% set models = graph.nodes.values()
         | selectattr('config.materialized', 'equalto', 'stream_table') %}
    {% for model in models %}
      {% set st_name = model.config.get('stream_table_name', model.name) %}
      {% set st_schema = model.config.get('stream_table_schema', 'public') %}
      {% set qualified = st_schema ~ '.' ~ st_name %}
      {% if pgstream_stream_table_exists(qualified) %}
        {{ pgstream_drop_stream_table(qualified) }}
        {% do dropped.append(qualified) %}
      {% endif %}
    {% endfor %}
    {{ log("pg_stream: dropped " ~ dropped | length ~ " dbt-managed stream table(s)", info=true) }}
  {% endif %}
{% endmacro %}
```

#### `drop_all_stream_tables_force` (nuclear — all stream tables)

Queries the pg_stream catalog directly. Drops **all** stream tables, including those
created outside dbt. Use with caution in shared environments.

```sql
{% macro drop_all_stream_tables_force() %}
  {% if execute %}
    {% set query %}
      SELECT pgs_schema || '.' || pgs_name AS qualified_name
      FROM pgstream.pgs_stream_tables
    {% endset %}
    {% set results = run_query(query) %}
    {% if results and results.rows | length > 0 %}
      {% for row in results.rows %}
        {{ pgstream_drop_stream_table(row['qualified_name']) }}
      {% endfor %}
      {{ log("pg_stream: force-dropped " ~ results.rows | length ~ " stream table(s)", info=true) }}
    {% else %}
      {{ log("pg_stream: no stream tables found to drop", info=true) }}
    {% endif %}
  {% endif %}
{% endmacro %}
```

### 6.4 `dbt test`

No special handling needed. Stream tables are standard PostgreSQL heap tables. All dbt
tests (schema tests, data tests, custom tests) work normally by querying the table.

The `__pgs_row_id` column is present but does not interfere with tests unless the user
explicitly selects `*` and checks column counts. Document this in the README.

### 6.5 `dbt docs generate`

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

---

## Phase 7 — Source Freshness Integration

### 7.1 Why native `dbt source freshness` doesn't work directly

dbt's `dbt source freshness` runs `SELECT MAX(loaded_at_field) FROM <source_table>`.
However, `last_refresh_at` lives in the **catalog table** (`pgstream.pgs_stream_tables`),
not on the stream table itself. Running `SELECT MAX(last_refresh_at) FROM order_totals`
would fail because that column doesn't exist on the stream table.

Overriding `collect_freshness` requires adapter-level Python code (Option B), which is
out of scope for a macro-only package.

### 7.2 Workaround: run-operation freshness check

Instead of native `dbt source freshness`, we provide a run-operation that queries
pg_stream's monitoring view directly.

File: `macros/hooks/source_freshness.sql`

```sql
{% macro pgstream_check_freshness(model_name=none, warn_seconds=600, error_seconds=1800) %}
  {#
    Check freshness of stream tables via pg_stream's monitoring view.
    If model_name is provided, check only that stream table.
    Otherwise, check all stream tables.

    Args:
      model_name (str|none): Specific stream table to check, or all if none
      warn_seconds (int): Staleness threshold for warnings (default: 600 = 10 min)
      error_seconds (int): Staleness threshold for errors (default: 1800 = 30 min)
  #}
  {% if execute %}
    {% set query %}
      SELECT
        pgs_name,
        pgs_schema,
        last_refresh_at,
        EXTRACT(EPOCH FROM (now() - data_timestamp))::int AS staleness_seconds,
        consecutive_errors
      FROM pgstream.pgs_stream_tables
      WHERE status = 'ACTIVE'
      {% if model_name is not none %}
        AND pgs_name = {{ dbt.string_literal(model_name) }}
      {% endif %}
    {% endset %}
    {% set results = run_query(query) %}
    {% for row in results.rows %}
      {% set name = row['pgs_schema'] ~ '.' ~ row['pgs_name'] %}
      {% set staleness = row['staleness_seconds'] %}
      {% if staleness is not none and staleness > error_seconds %}
        {{ log("ERROR: stream table '" ~ name ~ "' is stale (" ~ staleness ~ "s > " ~ error_seconds ~ "s)", info=true) }}
      {% elif staleness is not none and staleness > warn_seconds %}
        {{ log("WARN: stream table '" ~ name ~ "' is approaching staleness (" ~ staleness ~ "s > " ~ warn_seconds ~ "s)", info=true) }}
      {% else %}
        {{ log("OK: stream table '" ~ name ~ "' is fresh (" ~ staleness ~ "s)", info=true) }}
      {% endif %}
      {% if row['consecutive_errors'] > 0 %}
        {{ log("WARN: stream table '" ~ name ~ "' has " ~ row['consecutive_errors'] ~ " consecutive error(s)", info=true) }}
      {% endif %}
    {% endfor %}
  {% endif %}
{% endmacro %}
```

Usage:
```bash
# Check all stream tables
dbt run-operation pgstream_check_freshness

# Check a specific stream table with custom thresholds
dbt run-operation pgstream_check_freshness \
  --args '{model_name: order_totals, warn_seconds: 300, error_seconds: 900}'
```

### 7.3 Future: native source freshness (requires Option B adapter)

To enable `dbt source freshness` natively, Option B (custom adapter) could override
`collect_freshness()` in Python to query `pgstream.pgs_stream_tables.last_refresh_at`
directly. This would allow the standard `sources.yml` freshness config to work:

```yaml
# This YAML only works with Option B (custom adapter) — NOT with this macro package
sources:
  - name: pgstream
    schema: public
    freshness:
      warn_after: {count: 10, period: minute}
      error_after: {count: 30, period: minute}
    tables:
      - name: order_totals
```

For the macro-only approach, use the `pgstream_check_freshness` run-operation above.

---

## Phase 8 — Integration Tests

The `dbt-pgstream/integration_tests/` directory is a standalone dbt project that
validates all macros against a real PostgreSQL 18 instance with pg_stream installed.

### 8.1 Test project structure

```
dbt-pgstream/integration_tests/
├── dbt_project.yml
├── profiles.yml
├── packages.yml           # local: ../
├── models/
│   └── marts/
│       ├── order_totals.sql
│       └── schema.yml
├── seeds/
│   └── raw_orders.csv
└── tests/
    ├── assert_totals_correct.sql
    └── assert_no_errors.sql
```

### 8.2 `integration_tests/dbt_project.yml`

```yaml
name: 'dbt_pgstream_integration_tests'
version: '0.1.0'
config-version: 2

profile: 'integration_tests'

model-paths: ["models"]
seed-paths: ["seeds"]
test-paths: ["tests"]

clean-targets:
  - "target"
  - "dbt_packages"
```

### 8.3 `integration_tests/packages.yml`

```yaml
packages:
  - local: ../    # Install the parent dbt-pgstream package
```

### 8.4 `integration_tests/profiles.yml`

```yaml
integration_tests:
  target: default
  outputs:
    default:
      type: postgres
      host: "{{ env_var('PGHOST', 'localhost') }}"
      port: "{{ env_var('PGPORT', '5432') | as_number }}"
      user: "{{ env_var('PGUSER', 'postgres') }}"
      password: "{{ env_var('PGPASSWORD', 'postgres') }}"
      dbname: "{{ env_var('PGDATABASE', 'postgres') }}"
      schema: public
      threads: 1
```

### 8.5 `integration_tests/seeds/raw_orders.csv`

```csv
id,customer_id,amount,created_at
1,100,29.99,2026-01-15 10:30:00
2,101,49.50,2026-01-15 11:00:00
3,100,15.00,2026-01-15 12:15:00
4,102,99.99,2026-01-16 09:00:00
5,101,25.00,2026-01-16 10:30:00
6,100,75.00,2026-01-16 14:00:00
7,103,19.99,2026-01-17 08:45:00
8,102,50.00,2026-01-17 11:30:00
9,101,35.50,2026-01-17 13:00:00
10,100,42.00,2026-01-18 09:15:00
```

### 8.6 Test model: `integration_tests/models/marts/order_totals.sql`

```sql
{{ config(
    materialized='stream_table',
    schedule='1m',
    refresh_mode='DIFFERENTIAL'
) }}

SELECT
    customer_id,
    SUM(amount) AS total_amount,
    COUNT(*) AS order_count
FROM {{ ref('raw_orders') }}
GROUP BY customer_id
```

### 8.7 Test model schema: `integration_tests/models/marts/schema.yml`

```yaml
version: 2

models:
  - name: order_totals
    description: "Aggregated order totals per customer (stream table)"
    columns:
      - name: customer_id
        description: "Customer identifier"
        tests:
          - not_null
          - unique
      - name: total_amount
        description: "Sum of all order amounts"
        tests:
          - not_null
      - name: order_count
        description: "Number of orders"
        tests:
          - not_null
```

### 8.8 Data test: `integration_tests/tests/assert_totals_correct.sql`

```sql
-- Verify order_totals stream table matches expected aggregation.
-- Returns rows that are in expected but missing/different in actual.
-- An empty result set means the test passes.
WITH expected AS (
    SELECT
        customer_id,
        SUM(amount) AS total_amount,
        COUNT(*) AS order_count
    FROM {{ ref('raw_orders') }}
    GROUP BY customer_id
),
actual AS (
    SELECT customer_id, total_amount, order_count
    FROM {{ ref('order_totals') }}
)
SELECT e.*
FROM expected e
LEFT JOIN actual a
  ON e.customer_id = a.customer_id
  AND e.total_amount = a.total_amount
  AND e.order_count = a.order_count
WHERE a.customer_id IS NULL
```

### 8.9 Health test: `integration_tests/tests/assert_no_errors.sql`

```sql
-- Verify no stream tables have consecutive errors.
-- An empty result set means the test passes.
SELECT pgs_name, consecutive_errors
FROM pgstream.pgs_stream_tables
WHERE consecutive_errors > 0
```

### 8.10 Polling helper script

Instead of fragile `sleep` calls, use a polling script that waits until the stream
table is populated. This is more reliable in CI where timing varies.

File: `integration_tests/scripts/wait_for_populated.sh`

```bash
#!/usr/bin/env bash
# Wait for a stream table to be populated (is_populated = true).
# Usage: ./wait_for_populated.sh <stream_table_name> [timeout_seconds]
set -euo pipefail

NAME="${1:?Usage: wait_for_populated.sh <name> [timeout]}"
TIMEOUT="${2:-30}"
ELAPSED=0

while [ "$ELAPSED" -lt "$TIMEOUT" ]; do
  POPULATED=$(psql -tAc \
    "SELECT is_populated FROM pgstream.pgs_stream_tables WHERE pgs_name = '$NAME'")
  if [ "$POPULATED" = "t" ]; then
    echo "Stream table '$NAME' is populated after ${ELAPSED}s"
    exit 0
  fi
  sleep 1
  ELAPSED=$((ELAPSED + 1))
done

echo "ERROR: Stream table '$NAME' not populated after ${TIMEOUT}s" >&2
exit 1
```

### 8.11 Test for alter path (schedule change)

After the initial `dbt run`, modify the schedule config and re-run to verify the
alter path works. This can be done by having a second model file or by using
`dbt run-operation` to verify the schedule was updated:

```bash
# After initial dbt run, verify schedule is '1m'
psql -tAc "SELECT schedule FROM pgstream.pgs_stream_tables WHERE pgs_name = 'order_totals'"
# Should output: 1m

# TODO: Update model config to schedule='5m' and re-run
# (requires file modification between runs — implement as a shell script test)
```

> **Note:** Full automation of the alter path test requires modifying the model SQL
> file between runs. This is best done in a shell script wrapper around the dbt
> commands, not in dbt itself.

### 8.12 Test flow

```bash
cd dbt-pgstream/integration_tests

dbt deps                                # Install parent package (local: ../)
dbt seed                                # Load raw_orders.csv into PostgreSQL
dbt run                                 # Create stream tables via materialization
./scripts/wait_for_populated.sh order_totals 30  # Wait until populated
dbt test                                # Run schema + data tests
dbt run --full-refresh                  # Test drop/recreate path
./scripts/wait_for_populated.sh order_totals 30  # Wait again after recreate
dbt test                                # Verify still correct after full-refresh
dbt run-operation pgstream_refresh \
  --args '{model_name: order_totals}'   # Test manual refresh operation
dbt run-operation pgstream_check_freshness  # Test freshness check
dbt run-operation drop_all_stream_tables    # Test teardown (dbt-managed only)
```

---

## Phase 9 — CI Pipeline

Since the macros live in the pg_stream repo, dbt integration tests run as part of the
main CI pipeline alongside the Rust extension tests.

### 9.1 CI job for main workflow

Add a `dbt-integration` job to the existing `.github/workflows/ci.yml`:

```yaml
dbt-integration:
  runs-on: ubuntu-latest
  needs: [build]   # Ensure the pg_stream Docker image is built first
  strategy:
    matrix:
      dbt-version: ['1.6', '1.7', '1.8', '1.9']
    fail-fast: false
  services:
    postgres:
      image: pg-stream-e2e:latest    # Custom image with pg_stream
      ports: ['5432:5432']
      env:
        POSTGRES_PASSWORD: postgres
  steps:
    - uses: actions/checkout@v4

    - uses: actions/setup-python@v5
      with: { python-version: '3.11' }

    - name: Install dbt
      run: |
        pip install \
          "dbt-core~=${{ matrix.dbt-version }}.0" \
          "dbt-postgres~=${{ matrix.dbt-version }}.0"

    - name: Create pg_stream extension
      run: |
        PGPASSWORD=postgres psql -h localhost -U postgres -c "CREATE EXTENSION pg_stream;"

    - name: Run integration tests
      env:
        PGHOST: localhost
        PGPORT: '5432'
        PGUSER: postgres
        PGPASSWORD: postgres
        PGDATABASE: postgres
      run: |
        cd dbt-pgstream/integration_tests
        dbt deps
        dbt seed
        dbt run
        ./scripts/wait_for_populated.sh order_totals 30
        dbt test
        dbt run --full-refresh
        ./scripts/wait_for_populated.sh order_totals 30
        dbt test
        dbt run-operation pgstream_refresh --args '{model_name: order_totals}'
        dbt run-operation pgstream_check_freshness
        dbt run-operation drop_all_stream_tables
```

### 9.2 CI considerations

- **Docker build time:** The pg-stream Docker build compiles Rust — takes 10-15 min.
  Consider caching the Docker image via `docker/build-push-action` with GitHub Actions
  cache, or building it in a separate job and sharing via artifact.
- **Polling instead of sleep:** Use `wait_for_populated.sh` instead of `sleep`.
  CI environments vary in speed — polling `pgstream.pgs_stream_tables.is_populated`
  is deterministic and doesn't waste time on fast machines or fail on slow ones.
- **dbt version matrix:** Test against dbt-core 1.6 through 1.9 to catch compatibility
  issues. 1.6 is the minimum (for `subdirectory` support in `packages.yml`).
- **PostgreSQL 18 availability:** The Dockerfile uses `postgres:18` — ensure the
  base image is available on Docker Hub at CI time.
- **No separate CI workflow:** The dbt tests run inside the main pipeline, ensuring API
  changes in the Rust extension are immediately validated against the macros in the same PR.
- **Private repo auth:** If the pg_stream repo is private, users (and CI) need
  SSH keys or tokens configured for `dbt deps` to clone via git. Document this
  in the README.

---

## Phase 10 — Documentation

### 10.1 `dbt-pgstream/README.md`

Cover these sections:

1. **What is dbt-pgstream** — one-paragraph description
2. **Prerequisites** — PG 18, pg_stream extension, dbt Core ≥ 1.6
3. **Installation** — `packages.yml` snippet with git URL + `subdirectory`
4. **Quick Start** — minimal model example (config + SQL)
5. **Configuration Reference** — table of all config keys with defaults
6. **Operations** — `pgstream_refresh`, `drop_all_stream_tables`, `drop_all_stream_tables_force`
7. **Freshness Monitoring** — `pgstream_check_freshness` run-operation (note: native `dbt source freshness` not supported)
8. **Testing** — how stream tables interact with dbt test
9. **`__pgs_row_id` Column** — what it is, how to handle it
10. **Limitations** — known limitations table (link to this plan)
11. **Contributing** — link to development setup
12. **License** — Apache 2.0

### 10.2 CHANGELOG.md

Follow [Keep a Changelog](https://keepachangelog.com/) format:

```markdown
# Changelog

All notable changes to the dbt-pgstream package will be documented in this file.

## [Unreleased]

## [0.1.0] - 2026-XX-XX

### Added
- Custom `stream_table` materialization
- SQL API wrapper macros (create, alter, drop, refresh)
- Utility macros (stream_table_exists, get_stream_table_info)
- Freshness monitoring via `pgstream_check_freshness` run-operation
- `pgstream_refresh` and `drop_all_stream_tables` run-operations
- `drop_all_stream_tables_force` for dropping all stream tables (including non-dbt)
- Integration test suite with seed data and polling helper
- CI pipeline (dbt 1.6-1.9 version matrix in main repo workflow)
```

### 10.3 Inline macro documentation

All macros should have Jinja doc comments at the top:

```sql
{#
  pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize)

  Creates a new stream table via pgstream.create_stream_table().
  Called by the stream_table materialization on first run.

  Args:
    name (str): Stream table name (may be schema-qualified)
    query (str): The defining SQL query
    schedule (str): Refresh schedule (e.g., '1m', '5m', '0 */2 * * *')
    refresh_mode (str): 'FULL' or 'DIFFERENTIAL'
    initialize (bool): Whether to populate immediately on creation
#}
{% macro pgstream_create_stream_table(name, query, schedule, refresh_mode, initialize) %}
  ...
{% endmacro %}
```

---

## pg-stream SQL API Reference

Functions and catalog objects used by this package (all in `pgstream` schema):

### Functions

| Function | Signature | Used By |
|----------|-----------|---------|
| `create_stream_table` | `(name text, query text, schedule text DEFAULT '1m', refresh_mode text DEFAULT 'DIFFERENTIAL', initialize bool DEFAULT true) → void` | Materialization (create path). Note: `schedule` is actually `Option<&str>` in Rust — pass SQL `NULL` for CALCULATED schedule. |
| `alter_stream_table` | `(name text, schedule text DEFAULT NULL, refresh_mode text DEFAULT NULL, status text DEFAULT NULL) → void` | Materialization (update path) |
| `drop_stream_table` | `(name text) → void` | Materialization (full-refresh), `drop_all` operation |
| `refresh_stream_table` | `(name text) → void` | `refresh` run-operation |

### Catalog Objects

| Object | Type | Used By |
|--------|------|---------|
| `pgstream.pgs_stream_tables` | Table | `stream_table_exists()`, `get_stream_table_info()`, `drop_all_stream_tables()` |
| `pgstream.pg_stat_stream_tables` | View | `pgstream_check_freshness()` run-operation |
| `pgstream.pgs_stream_tables.consecutive_errors` | Column | `assert_no_errors` integration test |

---

## Limitations

| Limitation | Impact | Workaround |
|------------|--------|------------|
| No in-place query alteration | `alter_stream_table()` cannot change the defining query; must drop/recreate — brief data gap | The materialization handles this automatically |
| `__pgs_row_id` visible | Internal column appears in `SELECT *` and dbt docs | Document it; exclude in downstream models; Option B (adapter) can hide it |
| No `dbt snapshot` support | Snapshots use SCD Type-2 logic that doesn't apply to stream tables | Use a separate snapshot on the stream table as a regular table |
| No cross-database refs | Stream tables live in the same database as sources | Standard PostgreSQL limitation |
| Concurrent `dbt run` | Multiple `dbt run` invocations could race on create/drop of same stream table | Use dbt's `--target` or coordinate via CI |
| `dbt deps` payload | Users clone the full pg_stream repo (shallow, ~few MB) | Use `subdirectory` key; acceptable tradeoff |
| Query change detection | String comparison is sensitive to whitespace differences | dbt compiles deterministically; unnecessary recreations are safe |
| No native `dbt source freshness` | `loaded_at_field` cannot reference catalog columns; overriding `collect_freshness` requires adapter-level code | Use `pgstream_check_freshness` run-operation instead |
| PostgreSQL 18 required | PG 18 not yet GA — limits early adoption | Extension requirement, not dbt package issue |
| Extension is early-stage | pg_stream SQL API may evolve | Pin to pg_stream version; update macros as needed |
| Shared version tags | dbt package and Rust extension share git tags; a dbt-only fix requires a new extension release tag | Accept for now; extract to separate repo if this becomes a problem |

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
│   ├── CHANGELOG.md                      # Release history
│   ├── .gitignore                        # Ignore target/, dbt_packages/, logs/
│   ├── macros/
│   │   ├── materializations/
│   │   │   └── stream_table.sql          # ~80 lines — core materialization
│   │   ├── adapters/
│   │   │   ├── create_stream_table.sql   # ~15 lines
│   │   │   ├── alter_stream_table.sql    # ~25 lines
│   │   │   ├── drop_stream_table.sql     # ~10 lines
│   │   │   └── refresh_stream_table.sql  # ~10 lines
│   │   ├── hooks/
│   │   │   └── source_freshness.sql      # ~40 lines (check_freshness run-op)
│   │   ├── operations/
│   │   │   ├── refresh.sql               # ~8 lines
│   │   │   └── drop_all.sql              # ~35 lines (safe + force variants)
│   │   └── utils/
│   │       ├── stream_table_exists.sql   # ~20 lines
│   │       └── get_stream_table_info.sql # ~20 lines
│   └── integration_tests/
│       ├── dbt_project.yml
│       ├── profiles.yml
│       ├── packages.yml                  # local: ../
│       ├── models/
│       │   └── marts/
│       │       ├── order_totals.sql
│       │       └── schema.yml
│       ├── seeds/
│       │   └── raw_orders.csv
│       ├── tests/
│       │   ├── assert_totals_correct.sql
│       │   └── assert_no_errors.sql
│       └── scripts/
│           └── wait_for_populated.sh     # Polling helper for CI
├── Cargo.toml
└── ...
```

**Estimated total:** ~280 lines Jinja SQL macros + ~120 lines YAML config + ~100 lines test SQL/scripts

> No `.github/workflows/` directory inside `dbt-pgstream/` — CI lives in the main repo's
> workflow files and includes a `dbt-integration` job.

---

## Effort Estimate

| Phase | Effort |
|-------|--------|
| Phase 1 — Scaffolding | 1 hour |
| Phase 2 — SQL API wrappers | 2 hours |
| Phase 3 — Utility macros | 1 hour |
| Phase 4 — Custom materialization | 3 hours |
| Phase 5 — Model configuration | 0.5 hours |
| Phase 6 — Lifecycle operations | 1.5 hours |
| Phase 7 — Freshness monitoring | 1.5 hours |
| Phase 8 — Integration tests | 3 hours |
| Phase 9 — CI pipeline | 1.5 hours |
| Phase 10 — Documentation | 2 hours |
| **Total** | **~17 hours** |

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
dbt run-operation pgstream_refresh --args '{"model_name": "order_totals"}'

# Force drop + recreate
dbt run --select order_totals --full-refresh

# Check freshness (run-operation, not native dbt source freshness)
dbt run-operation pgstream_check_freshness

# Tear down dbt-managed stream tables
dbt run-operation drop_all_stream_tables

# Or tear down ALL stream tables (including non-dbt)
dbt run-operation drop_all_stream_tables_force
```

---

## Plan Changelog

Changes to this plan document, in reverse chronological order.

### 2026-02-24 — Review round 1

Fixes and improvements based on critique against the actual pg_stream codebase:

**Bugs fixed:**
1. **Source freshness rewritten (Phase 7):** Native `dbt source freshness` cannot work
   because `last_refresh_at` lives in the catalog table, not on the stream table itself.
   Overriding `collect_freshness` requires adapter-level code (Option B). Replaced with
   a `pgstream_check_freshness` run-operation that queries the catalog directly.
2. **Authoritative existence check (Phase 4):** Replaced `load_cached_relation(this)`
   with `pgstream_stream_table_exists()` as the authoritative check. The relation cache
   can be wrong if stream tables are created/dropped outside dbt.
3. **Double catalog lookup eliminated (Phase 2.2 + 4.1):** `alter_stream_table` now
   accepts a `current_info` parameter so the materialization can pass its already-fetched
   metadata instead of making a redundant SPI roundtrip.
4. **Schema-qualified catalog lookup (Phase 3):** Utility macros now filter on **both**
   `pgs_schema` AND `pgs_name`, matching how the Rust catalog layer queries
   (`WHERE pgs_schema = $1 AND pgs_name = $2`). Prevents ambiguity when two schemas
   have a stream table with the same name.
5. **NULL schedule handling (Phase 2.1):** `create_stream_table` wrapper now passes SQL
   `NULL` when `schedule` is `none`, enabling pg_stream's CALCULATED schedule behavior.
6. **Health test column name (Phase 8.9):** Fixed `name` → `pgs_name` to match the
   actual `pgs_stream_tables` catalog column.

**Missing coverage added:**
7. **`status` config key (Phase 5.2):** Users can now set `status: PAUSED` or
   `status: ACTIVE` in model config to pause/resume stream tables via `alter_stream_table`.
8. **`dbt build` discussion (Phase 6.1.1):** Documents how `dbt build` interacts with
   stream table models (DAG ordering, `initialize: false` caveat).
9. **Alter path test (Phase 8.11):** Notes for testing schedule/mode changes between runs.
10. **Polling instead of sleep (Phase 8.10, 8.12, 9.1):** Replaced fragile `sleep 5`
    with a `wait_for_populated.sh` polling script that checks `is_populated` in the catalog.

**Improvements:**
11. **Renamed `refresh` → `pgstream_refresh` (Phase 6.2):** Avoids name collisions with
    other packages or user macros.
12. **Safe drop as default (Phase 6.3):** `drop_all_stream_tables` now drops only
    dbt-managed stream tables (via `graph.nodes`). The catalog-based "nuclear" version is
    available as `drop_all_stream_tables_force`.
13. **Always schema-qualify (Phase 4.1):** Materialization now always constructs
    `st_schema ~ '.' ~ st_name` instead of special-casing `public`.
14. **Error handling note (Phase 2):** Documents how wrapper errors surface and suggests
    `{% call statement(...) %}` for production hardening.
15. **dbt version matrix (Phase 9.1):** Added 1.6 to the CI matrix (matches the stated
    minimum requirement).
16. **Versioning limitation:** Added shared-tag versioning concern to Limitations table.
17. **Native freshness limitation:** Added to Limitations table with workaround reference.
18. **`.gitignore` in file layout:** Added to prevent committing `target/`, `dbt_packages/`,
    `logs/` from integration tests.
19. **Private repo auth (Phase 9.2):** CI considerations now note SSH/token requirements
    for private repos.
20. **profiles.yml filter (Phase 8.4):** Fixed `| int` → `| as_number` (correct dbt Jinja
    filter name).
