# AGENTS.md — Development Guidelines for dbt-pg-stream

## Project Overview

dbt package providing a `stream_table` custom materialization for the
[pg-stream](https://github.com/grove/pg-stream) PostgreSQL extension. Pure Jinja
SQL macros — no Python adapter code. Works with stock `dbt-postgres`.

- **Language:** Jinja2 SQL (dbt macros)
- **Framework:** dbt Core ≥ 1.7, dbt-postgres adapter
- **Target database:** PostgreSQL 18 with pg_stream extension
- **Package name:** `dbt_pg_stream`
- **License:** Apache 2.0

Key docs: `plans/PLAN_DBT_MACRO.md` · `README.md` · `CHANGELOG.md`

## Directory Layout

```
macros/
├── materializations/
│   └── stream_table.sql           # Core custom materialization — entry point for dbt run
├── adapters/
│   ├── create_stream_table.sql    # Wraps pgstream.create_stream_table()
│   ├── alter_stream_table.sql     # Wraps pgstream.alter_stream_table()
│   ├── drop_stream_table.sql      # Wraps pgstream.drop_stream_table()
│   └── refresh_stream_table.sql   # Wraps pgstream.refresh_stream_table()
├── hooks/
│   └── source_freshness.sql       # Source freshness via pg_stat_stream_tables
├── operations/
│   ├── refresh.sql                # Manual refresh run-operation
│   └── drop_all.sql              # Drop all stream tables run-operation
└── utils/
    ├── stream_table_exists.sql    # Check catalog for stream table existence
    └── get_stream_table_info.sql  # Read stream table metadata from catalog

integration_tests/                 # Standalone dbt project for testing
├── dbt_project.yml
├── profiles.yml                   # Postgres → localhost:5432
├── packages.yml                   # Installs parent via local: ../
├── models/                        # Test stream table models
├── seeds/                         # CSV fixtures
└── tests/                         # Data tests / assertions
```

## Workflow — Always Do This

After any macro change, run the integration test suite:

```bash
cd integration_tests
dbt deps          # Install/update the parent package
dbt seed          # Load test fixtures
dbt run           # Create/update stream tables
sleep 5           # Wait for pg-stream scheduler
dbt test          # Run data + schema tests
```

After changes to the materialization logic, also test the full-refresh and alter paths:

```bash
dbt run --full-refresh    # Test drop/recreate
sleep 5
dbt test
```

Before committing, verify:
1. All integration tests pass
2. No Jinja compilation errors (`dbt compile` succeeds)
3. Macro naming is consistent with conventions below

## Jinja SQL Conventions

### Naming

- **Macros:** `pgstream_<action>` prefix (e.g., `pgstream_create_stream_table`)
- **Materialization:** `stream_table` (registered via `{% materialization stream_table, adapter='postgres' %}`)
- **Config keys:** snake_case (e.g., `stream_table_name`, `refresh_mode`)
- **Files:** One macro per file, filename matches macro name (without `pgstream_` prefix for adapters)

### Quoting and Safety

- Always use `dbt.string_literal()` to quote user-provided string values passed to SQL
- Never concatenate raw user input into SQL strings
- Guard `run_query()` calls with `{% if execute %}` to prevent parse-time execution

### Whitespace Control

- Use `{%- ... -%}` (trim) on variable assignments: `{%- set foo = config.get('bar') -%}`
- Use `{{ ... }}` (no trim) on output expressions that produce SQL
- Put `{# comments #}` on separate lines for readability

### Error Handling

- Use `exceptions.raise_compiler_error()` for fatal errors (missing config, invalid values)
- Use `log("pg-stream: ...", info=true)` for informational messages
- Check `run_query()` results before accessing `.rows`:
  ```sql
  {% set result = run_query(query) %}
  {% if result and result.rows | length > 0 %}
    {{ return(result.rows[0]) }}
  {% else %}
    {{ return(none) }}
  {% endif %}
  ```
- Return `none` (not empty dict) when a stream table is not found

### Materialization Structure

The `stream_table` materialization must handle exactly four cases:
1. **Full refresh + exists:** Drop, then create
2. **Does not exist:** Create
3. **Exists + query changed:** Drop, then create
4. **Exists + query unchanged:** Alter (schedule/mode only) or no-op

Always call `adapter.cache_new()` after creating a new relation.
Always call `run_hooks(pre_hooks)` / `run_hooks(post_hooks)`.
Always return `{'relations': [target_relation]}`.

### Config Defaults

| Key | Default | Valid Values |
|-----|---------|-------------|
| `schedule` | `'1m'` | pg-stream duration: `30s`, `1m`, `5m`, `1h`, etc. |
| `refresh_mode` | `'DIFFERENTIAL'` | `'DIFFERENTIAL'`, `'FULL'` |
| `initialize` | `true` | `true`, `false` |

## pg-stream SQL API

All functions live in the `pgstream` schema. The extension must be loaded via
`shared_preload_libraries = 'pg_stream'`.

| Function | Purpose |
|----------|---------|
| `pgstream.create_stream_table(name, query, schedule, refresh_mode, initialize)` | Create a new stream table |
| `pgstream.alter_stream_table(name, schedule, refresh_mode, status)` | Alter config (cannot change query) |
| `pgstream.drop_stream_table(name)` | Drop stream table and all resources |
| `pgstream.refresh_stream_table(name)` | Synchronous manual refresh |

Catalog: `pgstream.pgs_stream_tables` (metadata), `pgstream.pg_stat_stream_tables` (monitoring view)

## Testing

### Test tiers

| Tier | What | Command |
|------|------|---------|
| Compile check | Jinja syntax, macro resolution | `dbt compile` |
| Integration tests | Full lifecycle against real PG + pg-stream | `cd integration_tests && dbt seed && dbt run && dbt test` |
| Full-refresh path | Drop/recreate behavior | `dbt run --full-refresh && dbt test` |
| Operations test | Manual refresh, drop-all | `dbt run-operation refresh --args '{model_name: ...}'` |

### Test infrastructure

- Integration tests require PostgreSQL 18 with pg-stream extension
- Build the Docker image: `docker build -t pg-stream-e2e:latest -f tests/Dockerfile.e2e .`
  (from the [pg-stream repo](https://github.com/grove/pg-stream))
- Start: `docker run -d -e POSTGRES_PASSWORD=postgres -p 5432:5432 pg-stream-e2e:latest`
- Create extension: `psql -U postgres -c "CREATE EXTENSION pg_stream;"`

### Test naming

- Data tests: `assert_<what_is_tested>.sql` (e.g., `assert_totals_correct.sql`)
- Schema tests: defined in `schema.yml` alongside model definitions

## Do's and Don'ts

### Do

- Prefix all macros with `pgstream_` (except the materialization itself)
- Use `dbt.string_literal()` for all string parameters to pg-stream functions
- Guard all `run_query()` calls with `{% if execute %}`
- Log actions with `{{ log("pg-stream: ...", info=true) }}`
- Handle the "stream table not found" case gracefully (return `none`)
- Test both create and full-refresh paths
- Keep macros focused — one pg-stream function per adapter macro

### Don't

- Don't use `adapter.dispatch()` — this package is PostgreSQL-only
- Don't call pg-stream functions outside of macro wrappers
- Don't use `run_query()` without `{% if execute %}` guard
- Don't modify the `__pgs_row_id` column — it's managed by pg-stream
- Don't assume `ALTER` can change the defining query — it can't; must drop/recreate
- Don't use `println` or `print` — use `log()` for dbt-compatible output
- Don't add Python code — this is a macro-only package (Option A)

## Commit Messages

Follow conventional commits:
```
feat: add stream_table materialization
fix: handle schema-qualified names in existence check
test: add full-refresh integration test
docs: update config reference in README
chore: update dbt version constraint
```

## Code Review Checklist

- [ ] Macro names follow `pgstream_` convention
- [ ] All string params use `dbt.string_literal()`
- [ ] All `run_query()` calls guarded by `{% if execute %}`
- [ ] Materialization handles all four lifecycle cases
- [ ] Integration tests pass (create, test, full-refresh, test)
- [ ] Log messages are informative and prefixed with `pg-stream:`
- [ ] No raw SQL concatenation of user input
- [ ] CHANGELOG.md updated
