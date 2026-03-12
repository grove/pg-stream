# PLAN: dbt Getting Started Example Project

## Overview

Create a self-contained dbt example project at
`examples/getting_started/` (repo root level) that mirrors every SQL step in
`docs/GETTING_STARTED.md` using dbt models and seeds.  
The project serves three purposes:

1. **Tutorial** — a runnable companion to the getting-started guide so users
   can follow along with `dbt run` instead of raw SQL.
2. **Showcase** — demonstrates the `stream_table` materialization and schedule
   inheritance pattern in a realistic multi-layer DAG.
3. **Smoke test** — a lightweight additional CI target that validates the dbt
   materialization path against a real pg_trickle database.

---

## File Layout

```
examples/getting_started/
├── dbt_project.yml
├── profiles.yml
├── packages.yml
├── seeds/
│   ├── departments.csv
│   └── employees.csv
├── models/
│   ├── staging/
│   │   ├── stg_departments.sql
│   │   └── stg_employees.sql
│   └── orgchart/
│       ├── department_tree.sql
│       ├── department_stats.sql
│       └── department_report.sql
├── schema.yml
├── tests/
│   ├── assert_tree_paths_correct.sql
│   ├── assert_stats_headcount_matches.sql
│   ├── assert_report_payroll_matches.sql
│   └── assert_no_stream_table_errors.sql
└── scripts/
    ├── run_example.sh
    └── wait_for_populated.sh   (copy from dbt-pgtrickle/integration_tests)
```

---

## Step 1 — `dbt_project.yml`

```yaml
name: 'pgtrickle_getting_started'
version: '0.1.0'
config-version: 2

profile: 'getting_started'

require-dbt-version: [">=1.9.0", "<2.0.0"]

model-paths:  ["models"]
seed-paths:   ["seeds"]
test-paths:   ["tests"]

seeds:
  pgtrickle_getting_started:
    departments:
      +column_types:
        id:        integer
        parent_id: integer
      +post-hook: |
        DO $$
        BEGIN
          IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = to_regclass('{{ this }}') AND contype = 'p'
          ) THEN
            EXECUTE 'ALTER TABLE {{ this }} ADD PRIMARY KEY (id)';
          END IF;
        END
        $$;
    employees:
      +column_types:
        id:            integer
        department_id: integer
        salary:        numeric
      +post-hook: |
        DO $$
        BEGIN
          IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conrelid = to_regclass('{{ this }}') AND contype = 'p'
          ) THEN
            EXECUTE 'ALTER TABLE {{ this }} ADD PRIMARY KEY (id)';
          END IF;
        END
        $$;

models:
  pgtrickle_getting_started:
    staging:
      +materialized: view          # thin pass-throughs, not stream tables
    orgchart:
      +materialized: stream_table  # default for all orgchart models

clean-targets:
  - "target"
  - "dbt_packages"
```

**Design notes:**

- Seeds declare `column_types` so dbt doesn't infer all integers as `text`.
- Post-hooks add primary keys so pg_trickle can produce the best CDC and
  WAL-transition behaviour (mirrors the advice in the getting-started guide).
- Staging models are plain `view` — they don't need incremental maintenance.
- `orgchart` models default to `stream_table`; individual models override
  `schedule` and `refresh_mode` in `{{ config(...) }}` blocks.

---

## Step 2 — `profiles.yml`

```yaml
getting_started:
  target: default
  outputs:
    default:
      type: postgres
      host:     "{{ env_var('PGHOST',     'localhost') }}"
      port:     "{{ env_var('PGPORT',     '5432') | as_number }}"
      user:     "{{ env_var('PGUSER',     'postgres') }}"
      password: "{{ env_var('PGPASSWORD', 'postgres') }}"
      dbname:   "{{ env_var('PGDATABASE', 'postgres') }}"
      schema: public
      threads: 1
```

Identical pattern to `integration_tests/profiles.yml`. All connection
parameters are read from environment variables with localhost defaults so the
project works against a local pg_trickle instance out of the box.

---

## Step 3 — `packages.yml`

```yaml
packages:
  - git: "https://github.com/grove/pg-trickle"
    subdirectory: "dbt-pgtrickle"
    revision: main          # pin to a tag (e.g. v0.4.0) for production use
```

Installs `dbt-pgtrickle` directly from GitHub. The `subdirectory` key tells
dbt deps to treat `dbt-pgtrickle/` inside the repo as the package root.
Change `revision` to a release tag (e.g. `v0.4.0`) once the package is
formally tagged; use `main` during development.

Because the package is fetched from GitHub, the example project is fully
self-contained and can be cloned and run independently of the local repo
checkout. No filesystem sibling relationship is required.

---

## Step 4 — Seeds

### `seeds/departments.csv`

```csv
id,name,parent_id
1,Company,
2,Engineering,1
3,Sales,1
4,Operations,1
5,Backend,2
6,Frontend,2
7,Platform,2
```

Exactly the departments from the getting-started guide. `parent_id` for the
root row is left blank (dbt loads it as NULL). The post-hook adds `PRIMARY KEY
(id)`.

### `seeds/employees.csv`

```csv
id,name,department_id,salary
1,Alice,5,120000
2,Bob,5,115000
3,Charlie,6,110000
4,Diana,7,130000
5,Eve,3,95000
6,Frank,3,90000
7,Grace,4,100000
```

Seven employees from the guide. The post-hook adds `PRIMARY KEY (id)`.

**Foreign key constraints** between `employees.department_id →
departments.id` and `departments.parent_id → departments.id` are intentionally
omitted from seeds — dbt seeds cannot declare FK constraints without raw SQL
post-hooks, and adding them would complicate the seed load order. The guide
itself creates the tables without FK enforcement for simplicity.

---

## Step 5 — Staging Models

### `models/staging/stg_departments.sql`

```sql
{{ config(materialized='view') }}

SELECT
    id,
    name,
    parent_id
FROM {{ ref('departments') }}
```

### `models/staging/stg_employees.sql`

```sql
{{ config(materialized='view') }}

SELECT
    id,
    name,
    department_id,
    salary
FROM {{ ref('employees') }}
```

Both are thin `SELECT *`-style pass-throughs that make the downstream stream
table models independent of the seed table names. If a user later wants to
point the example at real source tables they change only the staging layer.

---

## Step 6 — Orgchart Stream Table Models

### `models/orgchart/department_tree.sql`

```sql
{{ config(
    materialized  = 'stream_table',
    schedule      = none,
    refresh_mode  = 'DIFFERENTIAL'
) }}

WITH RECURSIVE tree AS (
    SELECT
        id,
        name,
        parent_id,
        name      AS path,
        0         AS depth
    FROM {{ ref('stg_departments') }}
    WHERE parent_id IS NULL

    UNION ALL

    SELECT
        d.id,
        d.name,
        d.parent_id,
        tree.path || ' > ' || d.name  AS path,
        tree.depth + 1
    FROM {{ ref('stg_departments') }} d
    JOIN tree ON d.parent_id = tree.id
)
SELECT id, name, parent_id, path, depth FROM tree
```

`schedule = none` means CALCULATED mode — this table inherits the tightest
schedule of its downstream dependents. `refresh_mode = 'DIFFERENTIAL'` is
explicit to mirror the guide's discussion.

**Important:** dbt resolves `{{ ref('stg_departments') }}` to the real table
name at compile time. pg_trickle stores the compiled SQL in its catalog, so
the recursive CTE references the physical table. If the staging view is later
replaced by a different source, `dbt run` will call
`alter_stream_table(query => ...)` which migrates the definition in place.

### `models/orgchart/department_stats.sql`

```sql
{{ config(
    materialized  = 'stream_table',
    schedule      = none,
    refresh_mode  = 'DIFFERENTIAL'
) }}

SELECT
    t.id                                AS department_id,
    t.name                              AS department_name,
    t.path                              AS full_path,
    t.depth,
    COUNT(e.id)                         AS headcount,
    COALESCE(SUM(e.salary),  0)         AS total_salary,
    COALESCE(AVG(e.salary),  0)         AS avg_salary
FROM {{ ref('department_tree') }} t
LEFT JOIN {{ ref('stg_employees') }} e
       ON e.department_id = t.id
GROUP BY t.id, t.name, t.path, t.depth
```

Reads from the `department_tree` stream table via `ref()` — dbt records this
as a model dependency and pg_trickle records it as a DAG dependency. Both
systems know the correct refresh/build order.

### `models/orgchart/department_report.sql`

```sql
{{ config(
    materialized  = 'stream_table',
    schedule      = '1m',
    refresh_mode  = 'DIFFERENTIAL'
) }}

SELECT
    split_part(full_path, ' > ', 2)     AS division,
    SUM(headcount)                      AS total_headcount,
    SUM(total_salary)                   AS total_payroll
FROM {{ ref('department_stats') }}
WHERE depth >= 1
GROUP BY 1
```

The only model with an explicit `schedule = '1m'`. The two upstream CALCULATED
tables inherit this cadence automatically from the pg_trickle scheduler.

---

## Step 7 — `schema.yml`

```yaml
version: 2

seeds:
  - name: departments
    description: "Department hierarchy (self-referencing tree)"
    columns:
      - name: id
        description: "Department PK"
        tests: [not_null, unique]
      - name: name
        description: "Department display name"
        tests: [not_null]

  - name: employees
    description: "Employee roster"
    columns:
      - name: id
        description: "Employee PK"
        tests: [not_null, unique]
      - name: department_id
        description: "FK to departments"
        tests: [not_null]
      - name: salary
        description: "Annual salary"
        tests: [not_null]

models:
  - name: stg_departments
    description: "Thin pass-through view over the departments seed"

  - name: stg_employees
    description: "Thin pass-through view over the employees seed"

  - name: department_tree
    description: >
      Recursive CTE that flattens the department hierarchy.
      Maintained incrementally by pg_trickle (DIFFERENTIAL, CALCULATED schedule).
    columns:
      - name: id
        tests: [not_null, unique]
      - name: path
        tests: [not_null]
      - name: depth
        tests: [not_null]

  - name: department_stats
    description: >
      Per-department headcount and salary aggregates.
      Reads from department_tree — demonstrates stream-table-to-stream-table chaining.
    columns:
      - name: department_id
        tests: [not_null, unique]
      - name: headcount
        tests: [not_null]
      - name: total_salary
        tests: [not_null]

  - name: department_report
    description: >
      Top-level division rollup driven by a 1-minute schedule.
      Upstream CALCULATED tables inherit this cadence.
    columns:
      - name: division
        tests: [not_null, unique]
      - name: total_headcount
        tests: [not_null]
      - name: total_payroll
        tests: [not_null]
```

---

## Step 8 — Custom Tests

### `tests/assert_tree_paths_correct.sql`

```sql
-- Every department's path must start with 'Company' (the root).
-- Every non-root department's path must contain ' > '.
-- Returns rows that violate either condition. Empty result = pass.
SELECT id, name, path, depth
FROM {{ ref('department_tree') }}
WHERE
    path NOT LIKE 'Company%'
    OR (depth > 0 AND path NOT LIKE '% > %')
```

### `tests/assert_stats_headcount_matches.sql`

```sql
-- department_stats headcount must match a direct COUNT(*) from employees.
-- Returns rows where the incremental result diverges from the ground truth.
WITH ground_truth AS (
    SELECT
        department_id,
        COUNT(*)         AS headcount,
        SUM(salary)      AS total_salary
    FROM {{ ref('stg_employees') }}
    GROUP BY department_id
),
stream AS (
    SELECT department_id, headcount, total_salary
    FROM {{ ref('department_stats') }}
    WHERE headcount > 0
)
SELECT
    g.department_id,
    g.headcount           AS expected_headcount,
    s.headcount           AS actual_headcount,
    g.total_salary        AS expected_salary,
    s.total_salary        AS actual_salary
FROM ground_truth g
LEFT JOIN stream s USING (department_id)
WHERE
    s.department_id IS NULL
    OR g.headcount    <> s.headcount
    OR g.total_salary <> s.total_salary
```

### `tests/assert_report_payroll_matches.sql`

```sql
-- department_report payroll must equal the sum of direct employee
-- salaries for each top-level division.
WITH expected AS (
    SELECT
        split_part(t.path, ' > ', 2)  AS division,
        SUM(e.salary)                  AS total_payroll,
        COUNT(e.id)                    AS total_headcount
    FROM {{ ref('department_tree') }}   t
    JOIN {{ ref('stg_employees') }}     e  ON e.department_id = t.id
    WHERE t.depth >= 1
    GROUP BY 1
),
actual AS (
    SELECT division, total_payroll, total_headcount
    FROM {{ ref('department_report') }}
)
SELECT e.*
FROM expected e
LEFT JOIN actual a USING (division)
WHERE
    a.division IS NULL
    OR e.total_payroll    <> a.total_payroll
    OR e.total_headcount  <> a.total_headcount
```

### `tests/assert_no_stream_table_errors.sql`

```sql
-- No stream table managed by this project should have consecutive errors.
-- Returns failing stream tables. Empty result = pass.
SELECT pgt_name, consecutive_errors
FROM pgtrickle.pgt_stream_tables
WHERE consecutive_errors > 0
  AND pgt_name IN (
      'public.department_tree',
      'public.department_stats',
      'public.department_report'
  )
```

---

## Step 9 — Run Script (`scripts/run_example.sh`)

```bash
#!/usr/bin/env bash
# =============================================================================
# Run the pg_trickle Getting Started dbt example.
#
# Starts a pg_trickle E2E Docker container, runs the full dbt flow, waits
# for stream tables to populate, runs dbt tests, and cleans up.
#
# Usage:
#   ./examples/getting_started/scripts/run_example.sh
#   ./examples/getting_started/scripts/run_example.sh --keep-container
#
# Environment variables (same as integration_tests):
#   PGPORT          PostgreSQL port (default: 15433, avoids conflict with integration_tests)
#   CONTAINER_NAME  Docker container name (default: pgtrickle-getting-started)
#   SKIP_BUILD      Set to "1" to skip Docker image rebuild
#   KEEP_CONTAINER  Set to "1" to keep the container after the run
# =============================================================================
set -euo pipefail

PGPORT="${PGPORT:-15433}"
CONTAINER_NAME="${CONTAINER_NAME:-pgtrickle-getting-started}"
SKIP_BUILD="${SKIP_BUILD:-0}"
KEEP_CONTAINER="${KEEP_CONTAINER:-0}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
EXAMPLE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$EXAMPLE_DIR/../.." && pwd)"
E2E_IMAGE="pgtrickle-e2e:latest"

# ... same Docker start / psql wait / cleanup pattern as integration_tests/scripts/run_dbt_tests.sh ...

export PGHOST=localhost
export PGPORT
export PGUSER=postgres
export PGPASSWORD=postgres
export PGDATABASE=postgres

cd "$EXAMPLE_DIR"

echo "==> dbt deps"
dbt deps

echo "==> dbt seed"
dbt seed

echo "==> dbt run"
dbt run

echo "==> Waiting for stream tables to be populated..."
for ST in public.department_tree public.department_stats public.department_report; do
  bash "$SCRIPT_DIR/wait_for_populated.sh" "$ST" 30
done

echo "==> dbt test"
dbt test

echo "==> All checks passed."
```

Use port 15433 by default to avoid clashing with the integration test
container (15432).

---

## Step 10 — `scripts/wait_for_populated.sh`

Copy verbatim from
`dbt-pgtrickle/integration_tests/scripts/wait_for_populated.sh`.
Do not symlink — the example project should be independently runnable
without the rest of the repo checkout.

---

## Step 11 — justfile Target

Add to the root `justfile`:

```just
# Run the dbt Getting Started example project against a local pg_trickle container
test-dbt-getting-started:
    ./examples/getting_started/scripts/run_example.sh

# Run without rebuilding the Docker image
test-dbt-getting-started-fast:
    SKIP_BUILD=1 ./examples/getting_started/scripts/run_example.sh
```

---

## Step 12 — CI Integration

Add a new job to `.github/workflows/ci.yml` (or a dedicated `dbt-examples.yml`):

```yaml
dbt-getting-started:
  name: dbt getting-started example
  runs-on: ubuntu-latest
  # Run on push-to-main and schedule; skip on PRs (same policy as dbt integration tests)
  if: github.event_name != 'pull_request'
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-python@v5
      with:
        python-version: '3.12'
    - name: Install dbt
      run: pip install dbt-postgres==1.10.*
    - name: Run getting-started example
      run: ./examples/getting_started/scripts/run_example.sh
```

---

## Implementation Order

| Task | File(s) | Notes |
|------|---------|-------|
| GS-1 | `dbt_project.yml` | Project metadata, seed post-hooks, model defaults |
| GS-2 | `profiles.yml` | Connection config via env vars |
| GS-3 | `packages.yml` | GitHub URL reference to `grove/pg-trickle`, `subdirectory: dbt-pgtrickle` |
| GS-4 | `seeds/departments.csv` | 7 rows; blank parent_id for root |
| GS-5 | `seeds/employees.csv` | 7 rows |
| GS-6 | `models/staging/stg_departments.sql` | Pass-through view |
| GS-7 | `models/staging/stg_employees.sql` | Pass-through view |
| GS-8 | `models/orgchart/department_tree.sql` | Recursive CTE, schedule=none |
| GS-9 | `models/orgchart/department_stats.sql` | LEFT JOIN + GROUP BY, schedule=none |
| GS-10 | `models/orgchart/department_report.sql` | Rollup, schedule='1m' |
| GS-11 | `schema.yml` | Column docs + generic tests |
| GS-12 | `tests/assert_tree_paths_correct.sql` | Path structure invariant |
| GS-13 | `tests/assert_stats_headcount_matches.sql` | Headcount/salary ground-truth check |
| GS-14 | `tests/assert_report_payroll_matches.sql` | Division rollup ground-truth check |
| GS-15 | `tests/assert_no_stream_table_errors.sql` | Health check |
| GS-16 | `scripts/run_example.sh` | Docker + dbt orchestration |
| GS-17 | `scripts/wait_for_populated.sh` | Copy from integration_tests |
| GS-18 | `justfile` | `test-dbt-getting-started` targets |
| GS-19 | `.github/workflows/` | CI job (push-to-main + schedule) |

---

## Known Constraints and Edge Cases

### Recursive CTE and `ref()`

The `department_tree` model uses `{{ ref('stg_departments') }}` **twice** —
once in the base case and once in the recursive term. dbt's ref-resolution
expands both to the same physical table name, so the compiled SQL is a valid
recursive CTE with a self-join. This is correct and tested. No special
handling required.

### `packages.yml` GitHub subdirectory support

dbt's `git:` + `subdirectory:` package spec requires dbt-core ≥ 1.9. The
project already declares `require-dbt-version: [">=1.9.0", "<2.0.0"]` so
this is satisfied. During development it is convenient to temporarily switch
to `local: ../../dbt-pgtrickle` to test changes without pushing — document
this in a comment in `packages.yml`.
`stream_table` materialization already handles `NULL` schedule by calling
`create_stream_table(..., schedule => NULL)`. Verified in the existing
materialization code (`stream_table.sql` line ~27: `config.get('schedule',
'1m')`). For `none` to work correctly the materialization must pass `NULL` not
the string `'none'` — confirm this before GS-8/GS-9.

### Seed Load Order

dbt loads seeds in alphabetical filename order within a project. `departments`
(d) loads before `employees` (e), which is the correct order given that
`employees.department_id` references `departments.id`. Since we're not adding
FK constraints via seeds, this is not strictly required but is good practice.

### `dbt test` Timing

`dbt test` runs immediately after `dbt run`. The stream tables will be
populated on first `create_stream_table()` call (pg_trickle runs an initial
full refresh synchronously during creation). The `wait_for_populated.sh` check
is therefore largely redundant for the initial run but is useful if the script
is re-run against an existing container where the background scheduler might
not have fired yet.

### CALCULATED Schedule and `dbt run`

When `dbt run` calls `alter_stream_table()` on a subsequent run (model already
exists), the CALCULATED schedule for `department_tree` and `department_stats`
is recomputed by pg_trickle's scheduler, not by dbt. This is transparent —
dbt just re-runs the same `alter_stream_table(schedule => NULL)` call and
pg_trickle handles the propagation.

### Full Refresh (`dbt run --full-refresh`)

The `stream_table` materialization already handles `should_full_refresh()`:
it calls `drop_stream_table()` then `create_stream_table()`. This works
correctly with the getting-started models.

---

## Relationship to Existing Projects

| Project | Relationship |
|---------|-------------|
| `dbt-pgtrickle/` | Installed as a package via GitHub URL (`packages.yml`). Not a filesystem dependency. |
| `dbt-pgtrickle/integration_tests/` | Sibling in the same repo. `wait_for_populated.sh` is copied from here. `profiles.yml` pattern is reused. |
| `docs/GETTING_STARTED.md` | One-to-one correspondence: each Step N in the guide maps to a file or group of files in this project |
| `tests/e2e_*` | No overlap — dbt tests run via the dedicated `run_example.sh` script, not Testcontainers |

---

## Exit Criteria

- [ ] `dbt deps && dbt seed && dbt run && dbt test` passes against a local pg_trickle container
- [ ] `department_tree`, `department_stats`, `department_report` all show `is_populated = true`
- [ ] All four custom tests return empty result sets (no failures)
- [ ] `just test-dbt-getting-started` is documented in the justfile and works
- [ ] CI job passes on push-to-main
- [ ] A short "Try with dbt" callout is added to `docs/GETTING_STARTED.md` pointing at `dbt-pgtrickle/examples/getting_started/`
